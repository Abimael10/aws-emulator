// SPDX-License-Identifier: CC0-1.0

//! `DeleteGatewayRateLimit`, and the reads of the gateway's rate limits that follow it, end
//! to end over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="DeleteGatewayRateLimit,GetGatewayRateLimit,\
//! ListGatewayRateLimits")`): the delete takes the rate limit the path names off the gateway
//! that held it — it reports the limit `DELETING` and the record is gone once the clock owes
//! its removal, so a later `GetGatewayRateLimit` on that id is the modelled 404
//! `ResourceNotFoundException`, the listing no longer carries it, the limits beside it are
//! untouched, and the id is free for a new limit. A delete addressing nothing is that same
//! 404, and nothing a later call does brings a removed limit back.
//!
//! The rows differ along what the rule generalises over: whether the id was named by the
//! caller or minted by the service, how many dimension keys scope the limit and of which
//! published kind, how many rule entries it holds, which of the three rate metrics each
//! configures, which write put the limit on the gateway (the single create or the batch
//! write), and how many other limits the gateway holds beside the one deleted.

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

/// How long a row waits for a clock-owed transition or removal before failing. Far longer
/// than the span `src/state.rs` gives the gateway family, so a loaded or slow host cannot
/// fail a row: a row waits for the CONDITION it needs, never for a fixed time.
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

/// The sentence a missing RATE LIMIT is refused in: a limit is addressed by an id of its
/// gateway, so the refusal names that id.
fn rate_limit_not_found_message(rate_limit_id: &str) -> String {
    format!("Rate limit not found: {rate_limit_id}")
}

/// The one sentence this service refuses a lookup of a resource it does not hold in.
fn not_found_message(resource: &str) -> String {
    format!(
        "Failed to retrieve {resource} because it doesn't exist. Retry the request with a \
         different resource identifier."
    )
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

async fn batch_put(router: &MultiServiceRouter, gateway: &str, body: &Value) -> Answer {
    send(
        router,
        "PUT",
        &format!("/gateways/{gateway}/rate-limits/batch"),
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

async fn delete_rate_limit(router: &MultiServiceRouter, gateway: &str, rate_limit: &str) -> Answer {
    send(
        router,
        "DELETE",
        &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
        None,
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

async fn list_rate_limits(router: &MultiServiceRouter, gateway: &str) -> Answer {
    send(router, "GET", &format!("/gateways/{gateway}/rate-limits"), None).await
}

/// Create a gateway and hand back its `gatewayId`.
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
    text(&created.body, "gatewayId")
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

/// Look again until the rate limit is gone, which is what a cleanup loop polling for the
/// modelled 404 does. The deadline only catches a limit that is never removed at all.
async fn poll_until_gone(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    case: &str,
) -> Answer {
    let started = Instant::now();
    loop {
        let read = get_rate_limit(router, gateway, rate_limit).await;
        if read.status != 200 {
            return read;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: the deleted rate limit was still reported, last answer {} ({})",
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

/// One submitted rate limit, in the members the single create and the batch write share.
struct Submitted {
    /// The `rateLimitId` the request carries, or `None` to have one generated.
    rate_limit_id: Option<&'static str>,
    description: Option<&'static str>,
    dimension_keys: Vec<&'static str>,
    entries: Vec<Value>,
}

impl Submitted {
    fn body(&self) -> Value {
        let mut request = json!({
            "dimensionKeys": self.dimension_keys,
            "entries": self.entries,
        });
        if let Some(rate_limit_id) = self.rate_limit_id {
            request["rateLimitId"] = json!(rate_limit_id);
        }
        if let Some(description) = self.description {
            request["description"] = json!(description);
        }
        request
    }
}

/// Which write puts a row's rate limits on the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Written {
    /// `CreateGatewayRateLimit`, one limit per call.
    OneByOne,
    /// `BatchPutGatewayRateLimits`, the whole set in one call.
    Batch,
}

/// One row of the delete table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^([0-9a-zA-Z][-]?){1,48}$`).
    gateway: &'static str,
    written: Written,
    /// The limit the row deletes, first; the limits after it stay and must be untouched.
    submitted: Vec<Submitted>,
}

/// The rows. Every `dimensionKeys` value below is from `DimensionKey`'s own published
/// catalogue — `targetName | toolName | qualifiedModelId | $.context.iam.principal |
/// $.context.iam.sourceIdentity | $.context.jwt.<claim> |
/// $.context.request.header.<header> | $.context.request.body.<path>`
/// (`smithy_query(query="shape",shape="DimensionKeys")`) — including entries no recorded
/// request used. The rates include both published bounds of `RateConfig`'s `rate`
/// (`@range 0..10000000`).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a caller-named id, the gateway's only limit, created one by one",
            gateway: "deletelimitonly",
            written: Written::OneByOne,
            submitted: vec![Submitted {
                rate_limit_id: Some("per-target"),
                description: Some("acc per-target rate limit"),
                dimension_keys: vec!["targetName"],
                entries: vec![json!({
                    "dimensions": {"targetName": "orders"},
                    "requests": rate(100.0, "second"),
                })],
            }],
        },
        Row {
            case: "an id minted by the service, one of two limits, created one by one",
            gateway: "deletelimitminted",
            written: Written::OneByOne,
            submitted: vec![
                Submitted {
                    rate_limit_id: None,
                    description: None,
                    dimension_keys: vec!["toolName"],
                    entries: vec![json!({
                        "dimensions": {"toolName": "search"},
                        "tokens": rate(10_000_000.0, "minute"),
                    })],
                },
                Submitted {
                    rate_limit_id: Some("kept.limit_1"),
                    description: Some("the limit beside it"),
                    dimension_keys: vec!["qualifiedModelId"],
                    entries: vec![json!({
                        "dimensions": {"qualifiedModelId": "anthropic.claude"},
                        "connections": rate(0.0, "second"),
                    })],
                },
            ],
        },
        Row {
            case: "two dimension keys, three rule entries, all three rate metrics",
            gateway: "deletelimitthreerules",
            written: Written::OneByOne,
            submitted: vec![
                Submitted {
                    rate_limit_id: Some("scoped-by-two"),
                    description: Some(""),
                    dimension_keys: vec!["targetName", "toolName"],
                    entries: vec![
                        json!({
                            "dimensions": {"targetName": "orders", "toolName": "create"},
                            "requests": rate(5.0, "second"),
                        }),
                        json!({
                            "dimensions": {"targetName": "orders", "toolName": "read"},
                            "tokens": rate(50.0, "minute"),
                            "connections": rate(10.0, "second"),
                        }),
                        json!({
                            "dimensions": {"targetName": "*"},
                            "requests": rate(1.0, "minute"),
                        }),
                    ],
                },
                Submitted {
                    rate_limit_id: Some("kept-by-header"),
                    description: Some("per tenant"),
                    dimension_keys: vec!["$.context.request.header.x-tenant"],
                    entries: vec![json!({
                        "dimensions": {"$.context.request.header.x-tenant": "acme"},
                        "requests": rate(3.0, "second"),
                    })],
                },
            ],
        },
        Row {
            case: "a limit the batch write put on the gateway, one of three",
            gateway: "deletelimitbatched",
            written: Written::Batch,
            submitted: vec![
                Submitted {
                    rate_limit_id: Some("batched-jwt"),
                    description: Some("per caller"),
                    dimension_keys: vec!["$.context.jwt.sub"],
                    entries: vec![json!({
                        "dimensions": {"$.context.jwt.sub": "caller-1"},
                        "requests": rate(250.0, "minute"),
                    })],
                },
                Submitted {
                    rate_limit_id: Some("batched-principal"),
                    description: None,
                    dimension_keys: vec!["$.context.iam.principal"],
                    entries: vec![json!({
                        "dimensions": {"$.context.iam.principal": "AIDAEXAMPLE"},
                        "tokens": rate(7.5, "second"),
                    })],
                },
                Submitted {
                    rate_limit_id: Some("batched-body-path"),
                    description: Some("per account in the body"),
                    dimension_keys: vec!["$.context.request.body.account_id"],
                    entries: vec![json!({
                        "dimensions": {"$.context.request.body.account_id": "a-1"},
                        "connections": rate(2.0, "minute"),
                    })],
                },
            ],
        },
    ]
}

/// Put a row's rate limits on `gateway_id`, settled, and hand back their ids in submission
/// order.
async fn stored_limits(
    router: &MultiServiceRouter,
    gateway_id: &str,
    row: &Row,
) -> Vec<String> {
    let case = row.case;
    let mut ids = Vec::new();
    match row.written {
        Written::OneByOne => {
            for submitted in &row.submitted {
                let created = create_rate_limit(router, gateway_id, &submitted.body()).await;
                assert_eq!(
                    created.status, 201,
                    "case {case:?}: setup expected HTTP 201 from CreateGatewayRateLimit, got {} \
                     ({})",
                    created.status, created.body
                );
                ids.push(text(&created.body, "rateLimitId"));
            }
        }
        Written::Batch => {
            let body = json!({
                "rateLimits": row.submitted.iter().map(Submitted::body).collect::<Vec<_>>(),
            });
            let written = batch_put(router, gateway_id, &body).await;
            assert_eq!(
                written.status, 200,
                "case {case:?}: setup expected HTTP 200 from BatchPutGatewayRateLimits, got {} \
                 ({})",
                written.status, written.body
            );
            for submitted in &row.submitted {
                ids.push(
                    submitted
                        .rate_limit_id
                        .expect("a batched row names every id it submits")
                        .to_string(),
                );
            }
        }
    }
    for id in &ids {
        poll_until_status(router, gateway_id, id, "ACTIVE", case).await;
    }
    ids
}

/// The ids a listing of the gateway's rate limits reports.
fn listed_ids(body: &Value) -> Vec<String> {
    body.get("rateLimits")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| text(row, "rateLimitId"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// The delete takes the limit off the gateway: it is reported `DELETING`, then the read of
/// it is the modelled 404 and the listing no longer carries it, while the limits beside it
/// are untouched and the id it held is free again.
#[tokio::test]
async fn each_deleted_rate_limit_leaves_the_gateway_without_it() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let gateway_id = gateway(&router, row.gateway).await;
        let ids = stored_limits(&router, &gateway_id, &row).await;
        let (deleted, kept) = ids.split_first().expect("every row submits a limit");

        let accepted = delete_rate_limit(&router, &gateway_id, deleted).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected HTTP 200 from DeleteGatewayRateLimit, got {} ({})",
            accepted.status, accepted.body
        );
        assert_eq!(
            text(&accepted.body, "rateLimitId"),
            *deleted,
            "case {case:?}: the delete reports the id it was addressed by, got {}",
            accepted.body
        );
        // `GatewayRateLimitStatus` is `CREATING | ACTIVE | UPDATING | DELETING`
        // (`smithy_query(query="shape",shape="GatewayRateLimitStatus")`); a limit on its way
        // out reports the last of them.
        assert_eq!(
            text(&accepted.body, "status"),
            "DELETING",
            "case {case:?}: the delete reports the limit on its way out, got {}",
            accepted.body
        );

        let gone = poll_until_gone(&router, &gateway_id, deleted, case).await;
        assert_eq!(
            gone.status, 404,
            "case {case:?}: expected HTTP 404 from GetGatewayRateLimit after the delete, got {} \
             ({})",
            gone.status, gone.body
        );
        assert_eq!(
            error_code(&gone.body),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled not-found error, got {}",
            gone.body
        );
        assert_eq!(
            text(&gone.body, "message"),
            rate_limit_not_found_message(deleted),
            "case {case:?}: expected the refusal to name the id the path carried, got {}",
            gone.body
        );

        let listed = list_rate_limits(&router, &gateway_id).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200 from ListGatewayRateLimits, got {} ({})",
            listed.status, listed.body
        );
        let mut remaining = listed_ids(&listed.body);
        remaining.sort();
        let mut expected: Vec<String> = kept.to_vec();
        expected.sort();
        assert_eq!(
            remaining, expected,
            "case {case:?}: the listing holds exactly the limits beside the deleted one, got {}",
            listed.body
        );

        // The limits beside the deleted one are untouched: each still reads back by its own
        // id, in the terminal status it settled into.
        for id in kept {
            let read = get_rate_limit(&router, &gateway_id, id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the limit {id} beside the deleted one is still reported, got {} \
                 ({})",
                read.status, read.body
            );
            assert_eq!(
                text(&read.body, "status"),
                "ACTIVE",
                "case {case:?}: the limit {id} beside the deleted one keeps its status, got {}",
                read.body
            );
        }

        // The removal frees the id: a create may put a new limit under it, scoped however
        // the caller likes.
        let recreated = create_rate_limit(
            &router,
            &gateway_id,
            &json!({
                "rateLimitId": deleted,
                "description": "the limit that took the freed id",
                "dimensionKeys": ["$.context.iam.sourceIdentity"],
                "entries": [{
                    "dimensions": {"$.context.iam.sourceIdentity": "operator"},
                    "requests": rate(1.0, "second"),
                }],
            }),
        )
        .await;
        assert_eq!(
            recreated.status, 201,
            "case {case:?}: expected HTTP 201 re-creating a limit under the freed id {deleted}, \
             got {} ({})",
            recreated.status, recreated.body
        );
    }
}

/// A delete addressing nothing writes nothing and is the modelled 404: an id no limit of
/// the gateway holds names the limit it did not find, and an identifier no gateway holds is
/// refused for the gateway instead.
#[tokio::test]
async fn a_delete_addressing_nothing_is_the_modelled_not_found() {
    let router = router();
    let gateway_id = gateway(&router, "deletelimitmissing").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &json!({
            "rateLimitId": "held-limit",
            "dimensionKeys": ["targetName"],
            "entries": [{
                "dimensions": {"targetName": "orders"},
                "requests": rate(4.0, "second"),
            }],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the limit must be created, got {} ({})",
        created.status, created.body
    );

    // Each id is inside `GatewayRateLimitId`'s 2..64 length and its pattern
    // `^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$`, and none of them is the id the
    // gateway holds.
    for (case, rate_limit_id) in [
        ("a short id", "ab"),
        ("an id with the punctuation the pattern admits", "no.such_limit-9"),
        ("an id at the published length ceiling", &"z".repeat(64)[..]),
        ("the held id in a different case", "HELD-LIMIT"),
    ] {
        let refused = delete_rate_limit(&router, &gateway_id, rate_limit_id).await;
        assert_eq!(
            refused.status, 404,
            "case {case:?}: expected HTTP 404 deleting {rate_limit_id}, got {} ({})",
            refused.status, refused.body
        );
        assert_eq!(
            error_code(&refused.body),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled not-found error, got {}",
            refused.body
        );
        assert_eq!(
            text(&refused.body, "message"),
            rate_limit_not_found_message(rate_limit_id),
            "case {case:?}: expected the refusal to name the id the path carried, got {}",
            refused.body
        );
    }

    // A gateway nothing holds is refused for the GATEWAY, in the sentence this service
    // words a missing one in, before any limit is looked at.
    let missing_gateway =
        delete_rate_limit(&router, "no-such-gateway-0123456789", "held-limit").await;
    assert_eq!(
        missing_gateway.status, 404,
        "a delete on an unknown gateway is the modelled 404, got {} ({})",
        missing_gateway.status, missing_gateway.body
    );
    assert_eq!(
        text(&missing_gateway.body, "message"),
        not_found_message("gateway"),
        "a delete on an unknown gateway names the gateway, got {}",
        missing_gateway.body
    );

    // Nothing above touched the limit the gateway holds.
    let held = get_rate_limit(&router, &gateway_id, "held-limit").await;
    assert_eq!(
        held.status, 200,
        "the refused deletes left the held limit in place, got {} ({})",
        held.status, held.body
    );
}

/// `DeleteGatewayRateLimit` is `@idempotent`
/// (`smithy_query(query="operation",operation="DeleteGatewayRateLimit")`): a repeat neither
/// fails nor brings the limit back, whether it arrives while the removal is still owed or
/// after it came due.
#[tokio::test]
async fn a_repeated_delete_reports_the_same_removal() {
    let router = router();
    let gateway_id = gateway(&router, "deletelimitrepeated").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &json!({
            "rateLimitId": "twice-deleted",
            "dimensionKeys": ["toolName"],
            "entries": [{
                "dimensions": {"toolName": "search"},
                "requests": rate(9.0, "minute"),
            }],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the limit must be created, got {} ({})",
        created.status, created.body
    );
    poll_until_status(&router, &gateway_id, "twice-deleted", "ACTIVE", "the setup").await;

    let first = delete_rate_limit(&router, &gateway_id, "twice-deleted").await;
    assert_eq!(
        first.status, 200,
        "the first delete is accepted, got {} ({})",
        first.status, first.body
    );
    let second = delete_rate_limit(&router, &gateway_id, "twice-deleted").await;
    // Either the repeat still finds the limit on its way out, and reports that same
    // removal, or the clock already took the record and it is the modelled 404 — never a
    // server error, and never a second removal of something else.
    match second.status {
        200 => {
            assert_eq!(
                text(&second.body, "rateLimitId"),
                "twice-deleted",
                "the repeat reports the id it was addressed by, got {}",
                second.body
            );
            assert_eq!(
                text(&second.body, "status"),
                "DELETING",
                "the repeat reports the removal already under way, got {}",
                second.body
            );
        }
        404 => assert_eq!(
            text(&second.body, "message"),
            rate_limit_not_found_message("twice-deleted"),
            "the repeat after the removal came due names the id, got {}",
            second.body
        ),
        status => panic!("the repeated delete answered {status} ({})", second.body),
    }

    let gone = poll_until_gone(&router, &gateway_id, "twice-deleted", "the repeat").await;
    assert_eq!(
        gone.status, 404,
        "the twice-deleted limit is gone, got {} ({})",
        gone.status, gone.body
    );
}

/// An accepted removal stands: a later write of the same limit does not cancel it, and the
/// limit does not come back.
#[tokio::test]
async fn a_write_after_the_delete_does_not_bring_the_limit_back() {
    let router = router();
    let gateway_id = gateway(&router, "deletelimitthenwrite").await;
    let created = create_rate_limit(
        &router,
        &gateway_id,
        &json!({
            "rateLimitId": "deleted-then-updated",
            "description": "acc per-target rate limit",
            "dimensionKeys": ["targetName"],
            "entries": [{
                "dimensions": {"targetName": "orders"},
                "requests": rate(100.0, "second"),
            }],
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: the limit must be created, got {} ({})",
        created.status, created.body
    );
    poll_until_status(
        &router,
        &gateway_id,
        "deleted-then-updated",
        "ACTIVE",
        "the setup",
    )
    .await;

    let accepted = delete_rate_limit(&router, &gateway_id, "deleted-then-updated").await;
    assert_eq!(
        accepted.status, 200,
        "the delete is accepted, got {} ({})",
        accepted.status, accepted.body
    );

    let refused = update_rate_limit(
        &router,
        &gateway_id,
        "deleted-then-updated",
        &json!({
            "description": "acc per-target rate limit, relaxed to a per-minute budget",
            "entries": [{
                "dimensions": {"targetName": "orders"},
                "requests": rate(250.0, "minute"),
            }],
        }),
    )
    .await;
    // The update finds a limit on its way out — the modelled validation failure — or finds
    // nothing at all once the clock took the record: either way it is refused.
    assert!(
        refused.status == 400 || refused.status == 404,
        "the update of a deleted limit is refused, got {} ({})",
        refused.status,
        refused.body
    );

    let gone = poll_until_gone(&router, &gateway_id, "deleted-then-updated", "after the update")
        .await;
    assert_eq!(
        gone.status, 404,
        "the deleted limit stays gone after the refused update, got {} ({})",
        gone.status, gone.body
    );
    assert_eq!(
        text(&gone.body, "message"),
        rate_limit_not_found_message("deleted-then-updated"),
        "the read after the refused update names the id, got {}",
        gone.body
    );

    // The listing is the same answer: the gateway holds no rate limit at all.
    let listed = list_rate_limits(&router, &gateway_id).await;
    assert_eq!(
        listed_ids(&listed.body),
        Vec::<String>::new(),
        "the gateway holds no rate limit after the delete, got {}",
        listed.body
    );
}
