// SPDX-License-Identifier: CC0-1.0

//! `BatchPutGatewayRateLimits` beside the routes that resolve ONE rate limit of a gateway
//! by its id — `GetGatewayRateLimit`, `UpdateGatewayRateLimit`, `DeleteGatewayRateLimit` and
//! `ListGatewayRateLimits` — end to end over HTTP.
//!
//! Each row drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="BatchPutGatewayRateLimits,GetGatewayRateLimit,\
//! UpdateGatewayRateLimit,DeleteGatewayRateLimit,ListGatewayRateLimits")`): a write of a
//! gateway's rate limits that submits a SET of them is an upsert and removes nothing —
//! "atomically creates or updates multiple rate limits for a gateway. The operation updates
//! existing limits with matching keys and creates new limits for new keys" — so a limit the
//! submitted list does not name keeps its whole record and stays addressable by every route
//! that resolves one by id, and only a `DeleteGatewayRateLimit` of a limit takes it off the
//! gateway, after which those routes answer the modelled 404 `ResourceNotFoundException`
//! naming the `rateLimitId` the request carried.
//!
//! The rows differ along what the rule generalises over: how many limits the gateway holds
//! before the batch, whether each id was named by the caller or minted by the service, how
//! many dimension keys scope a limit and which published `DimensionKey` values they are,
//! which of the three rate metrics a rule entry configures, how many entries the batch
//! carries, and whether a submitted entry names a limit already held by its `rateLimitId`,
//! by the same dimension keys in another order, or names none of them at all.

use std::collections::BTreeSet;
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

/// The sentence a rate limit no id of the gateway resolves to is refused in: a limit is
/// addressed by an id OF its gateway, so the refusal names that id.
fn rate_limit_not_found_message(rate_limit_id: &str) -> String {
    format!("Rate limit not found: {rate_limit_id}")
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

async fn delete_rate_limit(router: &MultiServiceRouter, gateway: &str, rate_limit: &str) -> Answer {
    send(
        router,
        "DELETE",
        &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
        None,
    )
    .await
}

/// The `rateLimitId`s the gateway's listing reports, which is what reports a gateway's whole
/// set of rate limits.
async fn listed_ids(router: &MultiServiceRouter, gateway: &str) -> BTreeSet<String> {
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
        .map(|limits| {
            limits
                .iter()
                .map(|limit| text(limit, "rateLimitId"))
                .collect()
        })
        .unwrap_or_default()
}

/// Look again at one rate limit until it reports `wanted`, which is what a caller polling the
/// status field does. The deadline only catches a limit that never settles at all.
async fn poll_until_active(
    router: &MultiServiceRouter,
    gateway: &str,
    rate_limit: &str,
    case: &str,
) {
    let started = Instant::now();
    loop {
        let read = get_rate_limit(router, gateway, rate_limit).await;
        if read.status == 200 && text(&read.body, "status") == "ACTIVE" {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: {rate_limit} never reached ACTIVE, last answer {} ({})",
            read.status,
            read.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look again at one rate limit until the read of it no longer answers 200, which is what a
/// caller waiting out an accepted deletion does, and hand back that last answer.
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
            "case {case:?}: {rate_limit} was never taken off the gateway, last answer {} ({})",
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

/// One rule entry: the dimension values it scopes, and which of the three rate metrics
/// `LimitEntry` declares — `requests | tokens | connections`
/// (`smithy_query(query="shape",shape="LimitEntry")`) — it configures. `RateConfig` needs a
/// rate in 0..10000000 and a `Period`, whose values are `second | minute`
/// (`smithy_query(query="shape",shape="Period")`).
fn entry(dimensions: Value, metric: &str, rate: f64, period: &str) -> Value {
    let mut built = json!({"dimensions": dimensions});
    built[metric] = json!([{"rate": rate, "period": period}]);
    built
}

/// One rate limit as a request carries it, in the members the single create, the batch write
/// and the update share.
#[derive(Clone)]
struct Limit {
    /// The `rateLimitId` the request names, or `None` to have the service mint one.
    rate_limit_id: Option<&'static str>,
    description: Option<&'static str>,
    /// The scope of the limit, in `DimensionKey` values from that shape's own published
    /// catalogue — `targetName | toolName | qualifiedModelId | $.context.iam.principal |
    /// $.context.iam.sourceIdentity | $.context.jwt.<claim> |
    /// $.context.request.header.<header> | $.context.request.body.<path>`
    /// (`smithy_query(query="shape",shape="DimensionKeys")`).
    dimension_keys: Vec<&'static str>,
    entries: Vec<Value>,
}

impl Limit {
    fn body(&self) -> Value {
        let mut body = json!({"dimensionKeys": self.dimension_keys, "entries": self.entries});
        if let Some(rate_limit_id) = self.rate_limit_id {
            body["rateLimitId"] = json!(rate_limit_id);
        }
        if let Some(description) = self.description {
            body["description"] = json!(description);
        }
        body
    }
}

/// Which limit already on the gateway a submitted batch entry names, and by what.
#[derive(Clone, Copy)]
enum Names {
    /// The limit at this index of the row's standing set, by the `rateLimitId` the entry
    /// carries — which the model says the service "uses … for upsert matching against
    /// existing rate limits" (`smithy_query(query="shape",shape="BatchPutLimitEntry")`).
    ById(usize),
    /// The limit at this index of the row's standing set, by the dimension keys that scope
    /// it, the only other thing that identifies one limit of a gateway.
    ByKeys(usize),
}

impl Names {
    fn index(self) -> usize {
        match self {
            Self::ById(index) | Self::ByKeys(index) => index,
        }
    }
}

/// One entry of the batch a row submits.
struct Submitted {
    limit: Limit,
    /// The standing limit this entry upserts onto, or `None` when it names none of them and
    /// is created beside them.
    names: Option<Names>,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^[0-9a-zA-Z-]+$`).
    gateway: &'static str,
    /// The limits the single create puts on the gateway before the batch runs.
    standing: Vec<Limit>,
    /// The batch the row then submits.
    batch: Vec<Submitted>,
    /// The entries the row's later `UpdateGatewayRateLimit` of each standing limit submits,
    /// over that limit's own dimension keys, which are immutable after creation.
    updates: Vec<Vec<Value>>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one held limit over one scope, a batch over two scopes it does not hold",
            gateway: "batchkeepsthecreated",
            standing: vec![Limit {
                rate_limit_id: None,
                description: Some("per target"),
                dimension_keys: vec!["targetName"],
                entries: vec![entry(
                    json!({"targetName": "orders"}),
                    "requests",
                    25.0,
                    "second",
                )],
            }],
            batch: vec![
                Submitted {
                    limit: Limit {
                        rate_limit_id: None,
                        description: None,
                        dimension_keys: vec!["toolName"],
                        entries: vec![entry(
                            json!({"toolName": "search"}),
                            "tokens",
                            900.0,
                            "minute",
                        )],
                    },
                    names: None,
                },
                Submitted {
                    limit: Limit {
                        rate_limit_id: None,
                        description: Some("per model"),
                        dimension_keys: vec!["qualifiedModelId"],
                        entries: vec![entry(
                            json!({"qualifiedModelId": "anthropic.claude"}),
                            "requests",
                            5.0,
                            "second",
                        )],
                    },
                    names: None,
                },
            ],
            updates: vec![vec![entry(
                json!({"targetName": "orders"}),
                "requests",
                40.0,
                "second",
            )]],
        },
        Row {
            case: "two held limits, a batch that upserts the first by its id",
            gateway: "batchupsertsbyid",
            standing: vec![
                Limit {
                    rate_limit_id: Some("principal.limit"),
                    description: Some("per caller"),
                    dimension_keys: vec!["$.context.iam.principal"],
                    entries: vec![entry(
                        json!({"$.context.iam.principal": "arn:aws:iam::000000000000:user/one"}),
                        "tokens",
                        1200.0,
                        "minute",
                    )],
                },
                Limit {
                    rate_limit_id: None,
                    description: None,
                    dimension_keys: vec!["targetName", "toolName"],
                    entries: vec![entry(
                        json!({"targetName": "orders", "toolName": "search"}),
                        "connections",
                        8.0,
                        "second",
                    )],
                },
            ],
            batch: vec![
                Submitted {
                    limit: Limit {
                        rate_limit_id: None,
                        description: Some("per caller, raised"),
                        dimension_keys: vec!["$.context.iam.principal"],
                        entries: vec![entry(
                            json!({"$.context.iam.principal": "arn:aws:iam::000000000000:user/two"}),
                            "tokens",
                            2400.0,
                            "minute",
                        )],
                    },
                    names: Some(Names::ById(0)),
                },
                Submitted {
                    limit: Limit {
                        rate_limit_id: Some("tenant-header"),
                        description: None,
                        dimension_keys: vec!["$.context.request.header.x-tenant"],
                        entries: vec![entry(
                            json!({"$.context.request.header.x-tenant": "acme"}),
                            "requests",
                            60.0,
                            "minute",
                        )],
                    },
                    names: None,
                },
            ],
            updates: vec![
                vec![entry(
                    json!({"$.context.iam.principal": "arn:aws:iam::000000000000:user/three"}),
                    "tokens",
                    3600.0,
                    "minute",
                )],
                vec![entry(
                    json!({"targetName": "orders", "toolName": "search"}),
                    "connections",
                    16.0,
                    "second",
                )],
            ],
        },
        Row {
            case: "three held limits, a batch that upserts one by its keys in another order",
            gateway: "batchupsertsbykeys",
            standing: vec![
                Limit {
                    rate_limit_id: None,
                    description: None,
                    dimension_keys: vec!["qualifiedModelId"],
                    entries: vec![entry(
                        json!({"qualifiedModelId": "amazon.nova"}),
                        "requests",
                        12.0,
                        "second",
                    )],
                },
                Limit {
                    rate_limit_id: Some("jwt-subject"),
                    description: Some("per subject claim"),
                    dimension_keys: vec!["$.context.jwt.sub"],
                    entries: vec![entry(
                        json!({"$.context.jwt.sub": "user-17"}),
                        "tokens",
                        500.0,
                        "second",
                    )],
                },
                Limit {
                    rate_limit_id: None,
                    description: None,
                    dimension_keys: vec!["targetName", "toolName"],
                    entries: vec![entry(
                        json!({"targetName": "billing", "toolName": "invoice"}),
                        "connections",
                        3.0,
                        "second",
                    )],
                },
            ],
            batch: vec![
                Submitted {
                    limit: Limit {
                        rate_limit_id: None,
                        description: Some("per tool of a target"),
                        dimension_keys: vec!["toolName", "targetName"],
                        entries: vec![entry(
                            json!({"toolName": "invoice", "targetName": "billing"}),
                            "connections",
                            9.0,
                            "second",
                        )],
                    },
                    names: Some(Names::ByKeys(2)),
                },
                Submitted {
                    limit: Limit {
                        rate_limit_id: None,
                        description: None,
                        dimension_keys: vec!["$.context.request.body.model"],
                        entries: vec![entry(
                            json!({"$.context.request.body.model": "nova-pro"}),
                            "requests",
                            7.0,
                            "second",
                        )],
                    },
                    names: None,
                },
            ],
            updates: vec![
                vec![entry(
                    json!({"qualifiedModelId": "amazon.nova"}),
                    "requests",
                    24.0,
                    "second",
                )],
                vec![entry(
                    json!({"$.context.jwt.sub": "user-18"}),
                    "tokens",
                    750.0,
                    "second",
                )],
                vec![entry(
                    json!({"targetName": "billing", "toolName": "invoice"}),
                    "connections",
                    6.0,
                    "second",
                )],
            ],
        },
    ]
}

/// A batch write of a gateway's rate limits removes nothing: every limit it does not name is
/// still read back exactly as it stood, is still listed, and is still reachable by the update
/// and the delete routes — and once a limit really is deleted, a read of its id is the
/// modelled 404 naming that id.
#[tokio::test]
async fn a_batch_keeps_every_limit_it_does_not_name_addressable() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let gateway_id = gateway(&router, row.gateway).await;

        // What the gateway holds before the batch: each limit polled to its terminal status
        // and read back, so what the batch must leave alone is what a caller can see.
        let mut standing_ids: Vec<String> = Vec::new();
        let mut before: Vec<Value> = Vec::new();
        for limit in &row.standing {
            let created = create_rate_limit(&router, &gateway_id, &limit.body()).await;
            assert_eq!(
                created.status, 201,
                "case {case:?}: the rate limit must be created, got {} ({})",
                created.status, created.body
            );
            let rate_limit_id = text(&created.body, "rateLimitId");
            poll_until_active(&router, &gateway_id, &rate_limit_id, case).await;
            let read = get_rate_limit(&router, &gateway_id, &rate_limit_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: the held limit {rate_limit_id} must read back, got {} ({})",
                read.status, read.body
            );
            standing_ids.push(rate_limit_id);
            before.push(read.body);
        }

        // The batch, with the id of the limit an entry names by id filled in from the create
        // that minted or accepted it.
        let submitted: Vec<Value> = row
            .batch
            .iter()
            .map(|entry| {
                let mut body = entry.limit.body();
                if let Some(Names::ById(index)) = entry.names {
                    body["rateLimitId"] = json!(standing_ids[index]);
                }
                body
            })
            .collect();
        let batched = batch_put(&router, &gateway_id, &json!({"rateLimits": submitted})).await;
        assert_eq!(
            batched.status, 200,
            "case {case:?}: the batch write must answer 200, got {} ({})",
            batched.status, batched.body
        );
        let answered = batched
            .body
            .get("rateLimits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            answered.len(),
            row.batch.len(),
            "case {case:?}: the batch must answer one limit per submitted entry, got {}",
            batched.body
        );

        // The ids the batch created beside the limits already held, and the ids it upserted.
        let mut created_ids: Vec<String> = Vec::new();
        let named: BTreeSet<usize> = row
            .batch
            .iter()
            .filter_map(|entry| entry.names.map(Names::index))
            .collect();
        for (position, entry) in row.batch.iter().enumerate() {
            let answered_id = text(&answered[position], "rateLimitId");
            match entry.names {
                Some(names) => assert_eq!(
                    answered_id,
                    standing_ids[names.index()],
                    "case {case:?}: entry {position} names a limit the gateway holds, so it must \
                     be written under its id, got {}",
                    answered[position]
                ),
                None => {
                    assert!(
                        !standing_ids.contains(&answered_id),
                        "case {case:?}: entry {position} names no held limit, so it must get an \
                         id of its own, got {answered_id:?} among {standing_ids:?}"
                    );
                    created_ids.push(answered_id);
                }
            }
        }

        // The limits the batch did not name: the whole record, as it stood before the batch.
        for (index, rate_limit_id) in standing_ids.iter().enumerate() {
            let read = get_rate_limit(&router, &gateway_id, rate_limit_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: {rate_limit_id} must still read back after the batch, got {} ({})",
                read.status, read.body
            );
            if named.contains(&index) {
                // An upserted limit keeps the instant it was created at, and reports what the
                // batch submitted for it.
                let position = row
                    .batch
                    .iter()
                    .position(|entry| entry.names.map(Names::index) == Some(index))
                    .expect("the named limit has a submitted entry");
                assert_eq!(
                    read.body.get("createdAt"),
                    before[index].get("createdAt"),
                    "case {case:?}: the upsert of {rate_limit_id} must keep its creation \
                     instant, got {}",
                    read.body
                );
                assert_eq!(
                    read.body.get("entries"),
                    Some(&json!(row.batch[position].limit.entries)),
                    "case {case:?}: the upsert of {rate_limit_id} must report the entries the \
                     batch submitted, got {}",
                    read.body
                );
            } else {
                assert_eq!(
                    read.body, before[index],
                    "case {case:?}: the batch named no limit over {:?}, so {rate_limit_id} must \
                     read back unchanged, got {}",
                    row.standing[index].dimension_keys, read.body
                );
            }
        }

        // The gateway's whole set: everything it held, plus what the batch created.
        let wanted: BTreeSet<String> = standing_ids.iter().chain(&created_ids).cloned().collect();
        let listed = listed_ids(&router, &gateway_id).await;
        assert_eq!(
            listed, wanted,
            "case {case:?}: the listing must carry every limit the gateway holds after the batch"
        );

        // Every limit the batch left standing is still reachable by the UPDATE route, which a
        // limit the batch had dropped could not be.
        for (index, rate_limit_id) in standing_ids.iter().enumerate() {
            let updated = update_rate_limit(
                &router,
                &gateway_id,
                rate_limit_id,
                &json!({"entries": row.updates[index]}),
            )
            .await;
            assert_eq!(
                updated.status, 200,
                "case {case:?}: {rate_limit_id} must still be updatable, got {} ({})",
                updated.status, updated.body
            );
            assert_eq!(
                (
                    updated.body.get("entries"),
                    updated.body.get("createdAt"),
                    text(&updated.body, "rateLimitId")
                ),
                (
                    Some(&json!(row.updates[index])),
                    before[index].get("createdAt"),
                    rate_limit_id.clone()
                ),
                "case {case:?}: the update of {rate_limit_id} must report what it wrote on the \
                 limit it addressed, got {}",
                updated.body
            );
        }

        // And by the DELETE route — which is what a teardown of the limits a session created
        // does. Once the removal the delete accepted comes due, the read of that id is the
        // modelled 404 naming it, while every limit beside it is untouched.
        let retired = standing_ids[0].clone();
        let deleted = delete_rate_limit(&router, &gateway_id, &retired).await;
        assert_eq!(
            (
                deleted.status,
                text(&deleted.body, "rateLimitId"),
                text(&deleted.body, "status")
            ),
            (200, retired.clone(), "DELETING".to_string()),
            "case {case:?}: the delete of {retired} must report it on its way out, got {} ({})",
            deleted.status,
            deleted.body
        );
        let gone = poll_until_gone(&router, &gateway_id, &retired, case).await;
        assert_eq!(
            (
                gone.status,
                error_code(&gone.body),
                text(&gone.body, "message")
            ),
            (
                404,
                "ResourceNotFoundException".to_string(),
                rate_limit_not_found_message(&retired)
            ),
            "case {case:?}: the read of the deleted {retired} must name it, got {} ({})",
            gone.status,
            gone.body
        );
        for rate_limit_id in standing_ids.iter().skip(1).chain(&created_ids) {
            let read = get_rate_limit(&router, &gateway_id, rate_limit_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: {rate_limit_id} must survive the delete of {retired}, got {} ({})",
                read.status, read.body
            );
        }
        let remaining: BTreeSet<String> = standing_ids
            .iter()
            .skip(1)
            .chain(&created_ids)
            .cloned()
            .collect();
        assert_eq!(
            listed_ids(&router, &gateway_id).await,
            remaining,
            "case {case:?}: the listing must carry every limit but the deleted {retired}"
        );
    }
}
