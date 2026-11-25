// SPDX-License-Identifier: CC0-1.0

//! `BatchPutGatewayRateLimits` over a gateway that ALREADY holds rate limits, and the
//! reads, updates and deletes of those limits that follow it, end to end over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="BatchPutGatewayRateLimits,\
//! GetGatewayRateLimit,ListGatewayRateLimits,DeleteGatewayRateLimit")`): the batch write
//! UPSERTS the entries it submits onto the set the gateway holds — "atomically creates or
//! updates multiple rate limits for a gateway. The operation updates existing limits with
//! matching keys and creates new limits for new keys" — matching an entry against the limit
//! whose `rateLimitId` it carries ("if provided, the service uses it for upsert matching
//! against existing rate limits"), and otherwise against the limit scoped by the same
//! dimension keys, which are the only other thing that identifies one limit of a gateway
//! ("no two rate limits can share the same dimension keys"). It REMOVES nothing: a limit the
//! submitted list does not name keeps its own configuration, keeps being listed, and stays
//! addressable — a later `DeleteGatewayRateLimit` of it answers 200 with its `rateLimitId`
//! and status `DELETING`, which is what any teardown of the limits a session created does.
//!
//! The rows differ along what the rule generalises over: which write put the standing limits
//! on the gateway (the single create or an earlier batch), how many of them there are,
//! whether a submitted entry matches one by its id, by the same dimension keys, by those
//! keys in another order, or matches none of them at all, how many entries the batch
//! carries, which dimension keys scope each one and which of the three rate metrics its rule
//! entries configure.

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

/// How long a row waits for a clock-owed transition before failing. Far longer than the
/// span `src/state.rs` gives the gateway family, so a loaded or slow host cannot fail a
/// row: a row waits for the CONDITION it needs, never for a fixed time.
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

async fn list_rate_limits(router: &MultiServiceRouter, gateway: &str) -> Vec<Value> {
    let listed = send(
        router,
        "GET",
        &format!("/gateways/{gateway}/rate-limits"),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "the listing of a gateway's rate limits must answer, got {} ({})",
        listed.status, listed.body
    );
    listed
        .body
        .get("rateLimits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Look again at one rate limit until it reports `wanted`, which is what a caller polling
/// the status field does. The deadline only catches a limit that never settles at all.
async fn poll_until_status(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    wanted: &str,
    case: &str,
) {
    let started = Instant::now();
    loop {
        let read = get_rate_limit(router, gateway, rate_limit).await;
        if read.status == 200 && text(&read.body, "status") == wanted {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: {rate_limit} never reached {wanted}, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
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

/// A rate configuration: `RateConfig` needs a rate in 0..10000000 and a `Period`, whose
/// values are `second | minute` (`smithy_query(query="shape",shape="Period")`).
fn rate(value: f64, period: &str) -> Value {
    json!([{"rate": value, "period": period}])
}

/// One submitted rate limit, in the members the single create and the batch write share.
#[derive(Clone)]
struct Submitted {
    /// The `rateLimitId` the request carries, or `None` to have one generated.
    rate_limit_id: Option<&'static str>,
    description: Option<&'static str>,
    dimension_keys: Vec<&'static str>,
    entries: Vec<Value>,
}

impl Submitted {
    fn body(&self) -> Value {
        let mut body = json!({
            "dimensionKeys": self.dimension_keys,
            "entries": self.entries,
        });
        if let Some(rate_limit_id) = self.rate_limit_id {
            body["rateLimitId"] = json!(rate_limit_id);
        }
        if let Some(description) = self.description {
            body["description"] = json!(description);
        }
        body
    }
}

/// Which write put the limits the batch then upserts onto on the gateway.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Held {
    /// `CreateGatewayRateLimit`, one limit per call.
    Create,
    /// An earlier `BatchPutGatewayRateLimits`.
    Batch,
}

/// One row of the upsert table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^[0-9a-zA-Z-]+$`).
    gateway: &'static str,
    held_by: Held,
    /// The limits on the gateway before the batch runs.
    held: Vec<Submitted>,
    /// The batch the row then submits.
    batch: Vec<Submitted>,
    /// For each entry of `batch`, the index in `held` of the limit it upserts onto, or
    /// `None` when it matches none of them and is created beside them.
    matches: Vec<Option<usize>>,
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
            case: "a limit the single create put there, a batch over two other scopes",
            gateway: "upsertbesidecreated",
            held_by: Held::Create,
            held: vec![Submitted {
                rate_limit_id: None,
                description: Some("per target"),
                dimension_keys: vec!["targetName"],
                entries: vec![json!({
                    "dimensions": {"targetName": "orders"},
                    "requests": rate(25.0, "second"),
                })],
            }],
            batch: vec![
                Submitted {
                    rate_limit_id: None,
                    description: None,
                    dimension_keys: vec!["toolName"],
                    entries: vec![json!({
                        "dimensions": {"toolName": "search"},
                        "tokens": rate(100_000.0, "minute"),
                    })],
                },
                Submitted {
                    rate_limit_id: None,
                    description: Some("tokens by model"),
                    dimension_keys: vec!["qualifiedModelId"],
                    entries: vec![json!({
                        "dimensions": {"qualifiedModelId": "anthropic.claude"},
                        "tokens": rate(1_000.0, "minute"),
                    })],
                },
            ],
            matches: vec![None, None],
        },
        Row {
            case: "one entry matching a held limit by the id it carries, one new entry",
            gateway: "upsertbyid",
            held_by: Held::Create,
            held: vec![
                Submitted {
                    rate_limit_id: Some("held.by-id"),
                    description: Some("the first configuration"),
                    dimension_keys: vec!["toolName"],
                    entries: vec![json!({
                        "dimensions": {"toolName": "search"},
                        "requests": rate(7.0, "second"),
                    })],
                },
                Submitted {
                    rate_limit_id: Some("never-named"),
                    description: Some("nothing names this one again"),
                    dimension_keys: vec!["$.context.iam.sourceIdentity"],
                    entries: vec![json!({
                        "dimensions": {"$.context.iam.sourceIdentity": "alice"},
                        "connections": rate(3.0, "second"),
                    })],
                },
            ],
            batch: vec![
                Submitted {
                    rate_limit_id: Some("held.by-id"),
                    description: Some("the second configuration"),
                    dimension_keys: vec!["toolName"],
                    entries: vec![json!({
                        "dimensions": {"toolName": "search"},
                        "requests": rate(9.0, "minute"),
                    })],
                },
                Submitted {
                    rate_limit_id: Some("jwt_sub"),
                    description: Some("per caller"),
                    dimension_keys: vec!["$.context.jwt.sub"],
                    entries: vec![json!({
                        "dimensions": {"$.context.jwt.sub": "caller-1"},
                        "connections": rate(3.0, "second"),
                    })],
                },
            ],
            matches: vec![Some(0), None],
        },
        Row {
            case: "an entry carrying no id, matching a held limit by its dimension keys",
            gateway: "upsertbykeys",
            held_by: Held::Create,
            held: vec![Submitted {
                rate_limit_id: None,
                description: Some("per principal"),
                dimension_keys: vec!["$.context.iam.principal"],
                entries: vec![json!({
                    "dimensions": {"$.context.iam.principal": "arn:aws:iam::000000000000:role/Caller"},
                    "requests": rate(10_000_000.0, "minute"),
                })],
            }],
            batch: vec![Submitted {
                rate_limit_id: None,
                description: Some("per principal, adjusted"),
                dimension_keys: vec!["$.context.iam.principal"],
                entries: vec![json!({
                    "dimensions": {"$.context.iam.principal": "arn:aws:iam::000000000000:role/Caller"},
                    "requests": rate(42.0, "second"),
                })],
            }],
            matches: vec![Some(0)],
        },
        Row {
            case: "an entry whose dimension keys are a held limit's in another order",
            gateway: "upsertbypermutedkeys",
            held_by: Held::Create,
            held: vec![Submitted {
                rate_limit_id: None,
                description: Some("per target and model"),
                dimension_keys: vec!["targetName", "qualifiedModelId"],
                entries: vec![json!({
                    "dimensions": {"targetName": "orders"},
                    "requests": rate(5.0, "second"),
                })],
            }],
            batch: vec![Submitted {
                rate_limit_id: None,
                description: None,
                dimension_keys: vec!["qualifiedModelId", "targetName"],
                entries: vec![json!({
                    "dimensions": {"qualifiedModelId": "anthropic.claude"},
                    "tokens": rate(6.0, "minute"),
                })],
            }],
            matches: vec![Some(0)],
        },
        Row {
            case: "two limits an earlier batch wrote, one named again and one left alone",
            gateway: "upsertontobatched",
            held_by: Held::Batch,
            held: vec![
                Submitted {
                    rate_limit_id: Some("first-batched"),
                    description: Some("written by the first batch"),
                    dimension_keys: vec!["targetName", "toolName"],
                    entries: vec![
                        json!({
                            "dimensions": {"targetName": "orders", "toolName": "create"},
                            "requests": rate(5.0, "second"),
                        }),
                        json!({
                            "dimensions": {"targetName": "*"},
                            "requests": rate(1.0, "minute"),
                        }),
                    ],
                },
                Submitted {
                    rate_limit_id: Some("second-batched"),
                    description: Some("left out of the second batch"),
                    dimension_keys: vec!["$.context.request.header.x-tenant"],
                    entries: vec![json!({
                        "dimensions": {"$.context.request.header.x-tenant": "acme"},
                        "requests": rate(4.0, "second"),
                    })],
                },
            ],
            batch: vec![
                Submitted {
                    rate_limit_id: Some("first-batched"),
                    description: None,
                    dimension_keys: vec!["targetName", "toolName"],
                    entries: vec![json!({
                        "dimensions": {"targetName": "orders", "toolName": "read"},
                        "connections": rate(10.0, "second"),
                    })],
                },
                Submitted {
                    rate_limit_id: None,
                    description: Some("per account in the body"),
                    dimension_keys: vec!["$.context.request.body.account_id"],
                    entries: vec![json!({
                        "dimensions": {"$.context.request.body.account_id": "a-1"},
                        "connections": rate(2.0, "minute"),
                    })],
                },
            ],
            matches: vec![Some(0), None],
        },
    ]
}

/// A `rateLimitId` is well formed: `GatewayRateLimitId` is 2..64 characters matching
/// `^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$`.
fn well_formed_id(rate_limit_id: &str) -> bool {
    let characters: Vec<char> = rate_limit_id.chars().collect();
    (2..=64).contains(&characters.len())
        && characters
            .first()
            .is_some_and(|first| first.is_ascii_alphanumeric())
        && characters
            .last()
            .is_some_and(|last| last.is_ascii_alphanumeric())
        && characters
            .iter()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(*character))
}

/// Put `row.held` on the gateway through the write the row names, and hand back the id and
/// the creation instant of each one, waited out to `ACTIVE`.
async fn write_held(
    router: &MultiServiceRouter,
    gateway_id: &str,
    row: &Row,
) -> Vec<(String, Value)> {
    let case = row.case;
    let mut ids = Vec::new();
    match row.held_by {
        Held::Create => {
            for submitted in &row.held {
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
        Held::Batch => {
            let body = json!({
                "rateLimits": row.held.iter().map(Submitted::body).collect::<Vec<_>>(),
            });
            let written = batch_put(router, gateway_id, &body).await;
            assert_eq!(
                written.status, 200,
                "case {case:?}: setup expected HTTP 200 from BatchPutGatewayRateLimits, got {} \
                 ({})",
                written.status, written.body
            );
            for entry in written
                .body
                .get("rateLimits")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                ids.push(text(&entry, "rateLimitId"));
            }
        }
    }
    assert_eq!(
        ids.len(),
        row.held.len(),
        "case {case:?}: setup must put every held limit on the gateway, got {ids:?}"
    );
    let mut held = Vec::new();
    for id in ids {
        poll_until_status(router, gateway_id, &id, "ACTIVE", case).await;
        let read = get_rate_limit(router, gateway_id, &id).await;
        let created_at = read.body.get("createdAt").cloned().unwrap_or(Value::Null);
        assert!(
            !created_at.is_null(),
            "case {case:?}: setup dates the limit {id} it wrote, got {}",
            read.body
        );
        held.push((id, created_at));
    }
    held
}

/// The batch write upserts the entries it submits onto the gateway's set and removes
/// nothing: a limit it does not name keeps its own configuration, keeps being listed, and
/// is still deletable; a limit it does name is rewritten in place under the same id and the
/// same creation instant.
#[tokio::test]
async fn a_batch_upserts_onto_the_limits_the_gateway_already_holds() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let gateway_id = gateway(&router, row.gateway).await;
        let held = write_held(&router, &gateway_id, &row).await;

        let body = json!({"rateLimits": row.batch.iter().map(Submitted::body).collect::<Vec<_>>()});
        let written = batch_put(&router, &gateway_id, &body).await;
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
            row.batch.len(),
            "case {case:?}: the answer reports the entries the request submitted, got {}",
            written.body
        );

        // Every submitted entry is stored under the id the matching rule resolved for it:
        // the held limit's id where it matched one, and a new well-formed id otherwise.
        let mut created_ids = Vec::new();
        for (index, (entry, matched)) in reported.iter().zip(row.matches.iter()).enumerate() {
            let rate_limit_id = text(entry, "rateLimitId");
            match matched {
                Some(position) => assert_eq!(
                    rate_limit_id, held[*position].0,
                    "case {case:?}: entry {index} updates the limit it matches, got \
                     {rate_limit_id:?}"
                ),
                None => {
                    assert!(
                        well_formed_id(&rate_limit_id),
                        "case {case:?}: entry {index} is stored under a well-formed id, got \
                         {rate_limit_id:?}"
                    );
                    assert!(
                        !held.iter().any(|(id, _)| *id == rate_limit_id),
                        "case {case:?}: entry {index} matches no held limit, so it takes an id \
                         of its own, got {rate_limit_id:?}"
                    );
                    created_ids.push(rate_limit_id.clone());
                }
            }
            assert_eq!(
                text(entry, "status"),
                "ACTIVE",
                "case {case:?}: the batch applies its set atomically, so entry {index} is \
                 ACTIVE at once, got {entry}"
            );
            assert_eq!(
                entry.get("entries"),
                Some(&json!(row.batch[index].entries)),
                "case {case:?}: entry {index} reports the rule entries it was submitted with, \
                 got {entry}"
            );

            // The round trip: the stored limit reads back as the write reported it.
            let read = get_rate_limit(&router, &gateway_id, &rate_limit_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: entry {index} must read back, got {} ({})",
                read.status, read.body
            );
            for member in ["rateLimitId", "dimensionKeys", "entries", "status"] {
                assert_eq!(
                    read.body.get(member),
                    entry.get(member),
                    "case {case:?}: entry {index} reads back the {member} the write reported, \
                     got {}",
                    read.body
                );
            }
            assert_eq!(
                read.body.get("description").cloned(),
                row.batch[index].description.map(|text| json!(text)),
                "case {case:?}: entry {index} reads back its description, got {}",
                read.body
            );
            if let Some(position) = matched {
                assert_eq!(
                    read.body.get("createdAt"),
                    Some(&held[*position].1),
                    "case {case:?}: entry {index} updates a held limit, so it keeps the instant \
                     that limit was created, got {}",
                    read.body
                );
            }
        }

        // Every held limit the batch did not name is exactly as it was.
        for (position, (rate_limit_id, created_at)) in held.iter().enumerate() {
            if row.matches.contains(&Some(position)) {
                continue;
            }
            let standing = get_rate_limit(&router, &gateway_id, rate_limit_id).await;
            assert_eq!(
                (standing.status, text(&standing.body, "status").as_str()),
                (200, "ACTIVE"),
                "case {case:?}: the batch names no entry of {rate_limit_id}, so it is left where \
                 it was, got {} {} ({})",
                standing.status,
                text(&standing.body, "status"),
                standing.body
            );
            assert_eq!(
                standing.body.get("entries"),
                Some(&json!(row.held[position].entries)),
                "case {case:?}: {rate_limit_id} keeps the rule entries it was written with, got {}",
                standing.body
            );
            assert_eq!(
                standing.body.get("description").cloned(),
                row.held[position].description.map(|text| json!(text)),
                "case {case:?}: {rate_limit_id} keeps its own description, got {}",
                standing.body
            );
            assert_eq!(
                standing.body.get("createdAt"),
                Some(created_at),
                "case {case:?}: {rate_limit_id} keeps the instant it was created, got {}",
                standing.body
            );
        }

        // The resulting set is the union: every limit held before the batch, plus the
        // entries it created.
        let listed = list_rate_limits(&router, &gateway_id).await;
        let mut listed_ids: Vec<String> = listed
            .iter()
            .map(|entry| text(entry, "rateLimitId"))
            .collect();
        listed_ids.sort();
        let mut expected: Vec<String> = held
            .iter()
            .map(|(id, _)| id.clone())
            .chain(created_ids.iter().cloned())
            .collect();
        expected.sort();
        assert_eq!(
            listed_ids, expected,
            "case {case:?}: the listing reports the set the gateway ends up with"
        );

        // And a limit the batch never named is still addressable for deletion, which is
        // what a teardown of the limits a session created does.
        for (position, (rate_limit_id, _)) in held.iter().enumerate() {
            if row.matches.contains(&Some(position)) {
                continue;
            }
            let deleted = delete_rate_limit(&router, &gateway_id, rate_limit_id).await;
            assert_eq!(
                (
                    deleted.status,
                    text(&deleted.body, "rateLimitId"),
                    text(&deleted.body, "status"),
                ),
                (200, rate_limit_id.clone(), "DELETING".to_string()),
                "case {case:?}: deleting {rate_limit_id} answers the id it addressed and the \
                 status it moved to, got {} ({})",
                deleted.status,
                deleted.body
            );
            // `@idempotent`: a repeat reports that same limit while the removal the clock
            // owes is still outstanding, and the modelled 404 once the clock has taken the
            // limit off the gateway — never anything else.
            let again = delete_rate_limit(&router, &gateway_id, rate_limit_id).await;
            let repeat = (
                again.status,
                text(&again.body, "rateLimitId"),
                text(&again.body, "status"),
            );
            let still_deleting = repeat == (200, rate_limit_id.clone(), "DELETING".to_string());
            let removed = again.status == 404 && error_code(&again.body) == "ResourceNotFoundException";
            assert!(
                still_deleting || removed,
                "case {case:?}: a repeated delete of {rate_limit_id} reports it DELETING while \
                 its removal is owed and the modelled 404 once the clock has removed it, got {} \
                 ({})",
                again.status,
                again.body
            );
        }
    }
}

/// An entry that would scope a SECOND limit over a scope another limit of the gateway
/// already holds cannot be stored — "no two rate limits can share the same dimension keys"
/// is a rule about the set the gateway holds — so it is refused the modelled conflict, and
/// the batch writes nothing.
#[tokio::test]
async fn a_batch_that_would_double_a_scope_is_refused_and_writes_nothing() {
    let router = router();
    let gateway_id = gateway(&router, "upsertscopeconflict").await;

    let held = Submitted {
        rate_limit_id: Some("holds.the-scope"),
        description: Some("the limit that holds this scope"),
        dimension_keys: vec!["targetName"],
        entries: vec![json!({
            "dimensions": {"targetName": "orders"},
            "requests": rate(5.0, "second"),
        })],
    };
    let created = create_rate_limit(&router, &gateway_id, &held.body()).await;
    assert_eq!(
        created.status, 201,
        "setup: the held limit must be created, got {} ({})",
        created.status, created.body
    );

    // A DIFFERENT id over the same dimension keys: the batch matches no held limit by id,
    // and storing it would leave two limits of one gateway scoped the same way.
    let body = json!({"rateLimits": [Submitted {
        rate_limit_id: Some("doubles.the-scope"),
        description: None,
        dimension_keys: vec!["targetName"],
        entries: vec![json!({
            "dimensions": {"targetName": "orders"},
            "requests": rate(6.0, "second"),
        })],
    }
    .body()]});
    let refused = batch_put(&router, &gateway_id, &body).await;
    assert_eq!(
        (refused.status, error_code(&refused.body).as_str()),
        (409, "ConflictException"),
        "a second limit over a held scope is the modelled conflict, got {} {} ({})",
        refused.status,
        error_code(&refused.body),
        refused.body
    );
    assert!(
        text(&refused.body, "message").contains("holds.the-scope"),
        "the refusal names the limit that already holds the scope, got {:?}",
        text(&refused.body, "message")
    );

    let unwritten = get_rate_limit(&router, &gateway_id, "doubles.the-scope").await;
    assert_eq!(
        (unwritten.status, error_code(&unwritten.body).as_str()),
        (404, "ResourceNotFoundException"),
        "the refused batch writes nothing of its own, got {} ({})",
        unwritten.status,
        unwritten.body
    );
    let standing = get_rate_limit(&router, &gateway_id, "holds.the-scope").await;
    assert_eq!(
        standing.status, 200,
        "the refused batch leaves the held limit in place, got {} ({})",
        standing.status, standing.body
    );
    assert_eq!(
        standing.body.get("entries"),
        Some(&json!(held.entries)),
        "the refused batch leaves the held limit's configuration alone, got {}",
        standing.body
    );
}

/// A rate limit whose deletion the service has accepted is on its way out on the clock, so a
/// batch naming it is refused the modelled 400 rather than cancelling the removal — and the
/// limit stays on its way out.
#[tokio::test]
async fn a_batch_cannot_rewrite_a_limit_that_is_being_deleted() {
    let router = router();
    let gateway_id = gateway(&router, "upsertwhiledeleting").await;

    let held = Submitted {
        rate_limit_id: Some("on.its-way-out"),
        description: Some("deleted before the batch"),
        dimension_keys: vec!["toolName"],
        entries: vec![json!({
            "dimensions": {"toolName": "search"},
            "requests": rate(5.0, "second"),
        })],
    };
    let created = create_rate_limit(&router, &gateway_id, &held.body()).await;
    assert_eq!(
        created.status, 201,
        "setup: the held limit must be created, got {} ({})",
        created.status, created.body
    );
    let deleted = delete_rate_limit(&router, &gateway_id, "on.its-way-out").await;
    assert_eq!(
        (deleted.status, text(&deleted.body, "status")),
        (200, "DELETING".to_string()),
        "setup: the deletion must be accepted, got {} ({})",
        deleted.status,
        deleted.body
    );

    let body = json!({"rateLimits": [Submitted {
        rate_limit_id: Some("on.its-way-out"),
        description: Some("the batch tries to bring it back"),
        dimension_keys: vec!["toolName"],
        entries: vec![json!({
            "dimensions": {"toolName": "search"},
            "requests": rate(9.0, "minute"),
        })],
    }
    .body()]});
    let refused = batch_put(&router, &gateway_id, &body).await;
    if refused.status == 400 {
        assert_eq!(
            error_code(&refused.body),
            "ValidationException",
            "a limit on its way out takes no further configuration, got {} ({})",
            refused.status,
            refused.body
        );
        assert!(
            text(&refused.body, "message").contains("DELETING"),
            "the refusal names the status the caller found, got {:?}",
            text(&refused.body, "message")
        );
    } else {
        // The clock owes the removal a span of its own choosing, so a batch that arrives
        // after it came due addresses an id the gateway no longer holds and creates the
        // limit afresh. That is the only other answer this call has.
        assert_eq!(
            refused.status, 200,
            "the batch either is refused the limit's status or writes an id the gateway no \
             longer holds, got {} ({})",
            refused.status, refused.body
        );
    }
}
