// SPDX-License-Identifier: CC0-1.0

//! `DeleteGatewayRateLimit` over the statuses a rate limit can be in when the delete
//! arrives, and what its removal frees on the gateway that held it.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="DeleteGatewayRateLimit,CreateGatewayRateLimit")`):
//! the delete takes the rate limit the path names off the gateway whatever status that limit
//! is in when it arrives — the transient status the clock still owes a create or an update a
//! transition out of as much as the settled one — answering HTTP 200 with the id it was
//! addressed by and the limit reported `DELETING` (`GatewayRateLimitStatus` is
//! `CREATING | ACTIVE | UPDATING | DELETING`,
//! `smithy_query(query="shape",shape="GatewayRateLimitStatus")`); once the removal comes due
//! the limit is gone from the gateway together with everything only it held — the id it was
//! addressed by, and the dimension-key scope that made a second limit over the same scope the
//! modelled conflict.
//!
//! The rows differ along what the rule generalises over: which write put the limit on the
//! gateway and whether the delete arrives while the clock still owes that write a transition,
//! how many dimension keys of which published kind scope the limit, and which rate metric its
//! rule entries configure.

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
/// than the span the gateway family settles on, so a loaded or slow host cannot fail a row:
/// a row waits for the CONDITION it needs, never for a fixed time.
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

impl Answer {
    /// The error code `restJson1` puts in the body's `__type`.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("<no __type>")
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The string list a member carries, e.g. the `dimensionKeys` a read reports.
    fn strings(&self, member: &str) -> Vec<String> {
        self.body
            .get(member)
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| value.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
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
    created.text("gatewayId")
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
        if read.status == 200 && read.text("status") == wanted {
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

/// The sentence a missing RATE LIMIT is refused in: a limit is addressed by an id of its
/// gateway, so the refusal names that id.
fn rate_limit_not_found_message(rate_limit_id: &str) -> String {
    format!("Rate limit not found: {rate_limit_id}")
}

/// The ids a listing of the gateway's rate limits reports.
fn listed_ids(body: &Value) -> Vec<String> {
    body.get("rateLimits")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    row.get("rateLimitId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// What the gateway's rate limit has been through by the time the row deletes it.
#[derive(Debug, Clone, Copy)]
enum Prepared {
    /// `CreateGatewayRateLimit`, with the delete sent straight after it — so the delete
    /// arrives while the clock may still owe the create its transition out of `CREATING`.
    JustCreated,
    /// `CreateGatewayRateLimit`, waited out until it reports the settled `ACTIVE`.
    Settled,
    /// `CreateGatewayRateLimit`, settled, then `UpdateGatewayRateLimit`, with the delete sent
    /// straight after it — so the delete arrives while the clock may still owe the update its
    /// transition out of `UPDATING`.
    JustUpdated,
    /// `BatchPutGatewayRateLimits`, the atomic write, which leaves its entries in force at
    /// once.
    Batched,
}

/// One row of the delete table.
struct Row {
    case: &'static str,
    /// The gateway name the row creates (`GatewayName` is `^([0-9a-zA-Z][-]?){1,48}$`).
    gateway: &'static str,
    prepared: Prepared,
    rate_limit_id: &'static str,
    dimension_keys: Vec<&'static str>,
    entries: Vec<Value>,
}

/// The rows. Every `dimensionKeys` value below is from `DimensionKey`'s own published
/// catalogue — `targetName | toolName | qualifiedModelId | $.context.iam.principal |
/// $.context.iam.sourceIdentity | $.context.jwt.<claim> | $.context.request.header.<header> |
/// $.context.request.body.<path>` (`smithy_query(query="shape",shape="DimensionKey")`) —
/// including entries no recorded request used.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "deleted while the clock may still owe the create its transition",
            gateway: "dellimitcreating",
            prepared: Prepared::JustCreated,
            rate_limit_id: "while-creating",
            dimension_keys: vec!["toolName"],
            entries: vec![json!({
                "dimensions": {"toolName": "search"},
                "requests": rate(25.0, "second"),
            })],
        },
        Row {
            case: "deleted after it settled into its terminal status",
            gateway: "dellimitsettled",
            prepared: Prepared::Settled,
            rate_limit_id: "after.settling_1",
            dimension_keys: vec!["qualifiedModelId"],
            entries: vec![json!({
                "dimensions": {"qualifiedModelId": "anthropic.claude"},
                "tokens": rate(1_250.5, "minute"),
            })],
        },
        Row {
            case: "deleted while the clock may still owe the update its transition",
            gateway: "dellimitupdating",
            prepared: Prepared::JustUpdated,
            rate_limit_id: "while-updating",
            dimension_keys: vec!["targetName", "toolName"],
            entries: vec![
                json!({
                    "dimensions": {"targetName": "orders", "toolName": "create"},
                    "connections": rate(4.0, "second"),
                }),
                json!({
                    "dimensions": {"targetName": "*"},
                    "requests": rate(0.0, "minute"),
                }),
            ],
        },
        Row {
            case: "deleted after the atomic batch write put it in force",
            gateway: "dellimitbatched",
            prepared: Prepared::Batched,
            rate_limit_id: "from-the-batch",
            dimension_keys: vec!["$.context.jwt.sub"],
            entries: vec![json!({
                "dimensions": {"$.context.jwt.sub": "caller-1"},
                "tokens": rate(10_000_000.0, "second"),
            })],
        },
    ]
}

impl Row {
    /// The body the single create and the batch entry share for this row's limit.
    fn body(&self) -> Value {
        json!({
            "rateLimitId": self.rate_limit_id,
            "dimensionKeys": self.dimension_keys,
            "entries": self.entries,
        })
    }

    /// Put this row's rate limit on `gateway_id` and leave it in the status the row's
    /// `prepared` describes.
    async fn store(&self, router: &MultiServiceRouter, gateway_id: &str) {
        let case = self.case;
        match self.prepared {
            Prepared::Batched => {
                let written =
                    batch_put(router, gateway_id, &json!({"rateLimits": [self.body()]})).await;
                assert_eq!(
                    written.status, 200,
                    "case {case:?}: setup expected HTTP 200 from BatchPutGatewayRateLimits, got \
                     {} ({})",
                    written.status, written.body
                );
                // The atomic write leaves its entries in force at once, so this row's limit is
                // in the settled status from the instant of the write — the other end of the
                // range of statuses a delete can arrive on.
                assert_eq!(
                    written
                        .body
                        .get("rateLimits")
                        .and_then(Value::as_array)
                        .and_then(|rows| rows.first())
                        .and_then(|row| row.get("status"))
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                    "ACTIVE",
                    "case {case:?}: the batch write reports its entry in force, got {}",
                    written.body
                );
            }
            Prepared::JustCreated | Prepared::Settled | Prepared::JustUpdated => {
                let created = create_rate_limit(router, gateway_id, &self.body()).await;
                assert_eq!(
                    created.status, 201,
                    "case {case:?}: setup expected HTTP 201 from CreateGatewayRateLimit, got {} \
                     ({})",
                    created.status, created.body
                );
                // The create's OWN answer is rendered at the instant it stored the limit, so
                // it reports the transient status the clock owes that limit a transition out
                // of whatever the host's speed: a row that means to delete a limit the clock
                // still owes a transition fails here rather than quietly becoming the settled
                // row.
                assert_eq!(
                    created.text("status"),
                    "CREATING",
                    "case {case:?}: the create reports the status the clock owes a transition \
                     out of, got {}",
                    created.body
                );
                if matches!(self.prepared, Prepared::Settled | Prepared::JustUpdated) {
                    poll_until_status(router, gateway_id, self.rate_limit_id, "ACTIVE", case).await;
                }
                if matches!(self.prepared, Prepared::JustUpdated) {
                    let updated = update_rate_limit(
                        router,
                        gateway_id,
                        self.rate_limit_id,
                        &json!({"entries": self.entries}),
                    )
                    .await;
                    assert_eq!(
                        updated.status, 200,
                        "case {case:?}: setup expected HTTP 200 from UpdateGatewayRateLimit, got \
                         {} ({})",
                        updated.status, updated.body
                    );
                    // As for the create: the update's own answer carries the transient status
                    // it put the limit in, so this row is known to have reached that state.
                    assert_eq!(
                        updated.text("status"),
                        "UPDATING",
                        "case {case:?}: the update reports the status the clock owes a \
                         transition out of, got {}",
                        updated.body
                    );
                }
            }
        }
    }
}

/// The delete takes the limit off the gateway whatever status the limit is in when it
/// arrives: it answers 200 reporting that limit `DELETING`, and once the removal comes due a
/// read of the limit is the modelled 404 and the gateway lists it no more.
#[tokio::test]
async fn a_delete_is_accepted_whatever_status_the_limit_is_in() {
    for row in rows() {
        let case = row.case;
        let router = router();
        let gateway_id = gateway(&router, row.gateway).await;
        row.store(&router, &gateway_id).await;

        let accepted = delete_rate_limit(&router, &gateway_id, row.rate_limit_id).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?}: expected HTTP 200 from DeleteGatewayRateLimit, got {} ({})",
            accepted.status, accepted.body
        );
        assert_eq!(
            accepted.text("rateLimitId"),
            row.rate_limit_id,
            "case {case:?}: the delete reports the id it was addressed by, got {}",
            accepted.body
        );
        assert_eq!(
            accepted.text("status"),
            "DELETING",
            "case {case:?}: the delete reports the limit on its way out, got {}",
            accepted.body
        );

        let gone = poll_until_gone(&router, &gateway_id, row.rate_limit_id, case).await;
        assert_eq!(
            (gone.status, gone.error_code(), gone.text("message")),
            (
                404,
                "ResourceNotFoundException".to_string(),
                rate_limit_not_found_message(row.rate_limit_id)
            ),
            "case {case:?}: expected the modelled not-found naming the id the path carried, got \
             {} ({})",
            gone.status,
            gone.body
        );

        let listed = list_rate_limits(&router, &gateway_id).await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected HTTP 200 from ListGatewayRateLimits, got {} ({})",
            listed.status, listed.body
        );
        assert_eq!(
            listed_ids(&listed.body),
            Vec::<String>::new(),
            "case {case:?}: the gateway lists no rate limit after its only one went, got {}",
            listed.body
        );
    }
}

/// The removal frees the dimension-key scope the limit held: while the limit is there a
/// second limit over the same keys is the modelled conflict, and once the removal has come
/// due the same keys take a new limit, which reads back scoped by them.
#[tokio::test]
async fn the_removal_frees_the_dimension_keys_the_limit_held() {
    // One scope per row, each from `DimensionKey`'s published catalogue
    // (`smithy_query(query="shape",shape="DimensionKey")`), differing in how many keys scope
    // the limit and of which kind: a fixed dimension name, two of them, and a context-path
    // expression no recorded request used. `DimensionKeys` is `@length 1..10`.
    let scopes = [
        (
            "one fixed dimension name",
            "scopefreedone",
            vec!["toolName"],
            json!({"dimensions": {"toolName": "search"}, "requests": rate(12.0, "second")}),
        ),
        (
            "two fixed dimension names",
            "scopefreedtwo",
            vec!["targetName", "toolName"],
            json!({
                "dimensions": {"targetName": "orders", "toolName": "create"},
                "tokens": rate(99.0, "minute"),
            }),
        ),
        (
            "a request-header context path",
            "scopefreedheader",
            vec!["$.context.request.header.x-tenant"],
            json!({
                "dimensions": {"$.context.request.header.x-tenant": "acme"},
                "connections": rate(3.0, "second"),
            }),
        ),
    ];

    for (case, gateway_name, dimension_keys, entry) in scopes {
        let router = router();
        let gateway_id = gateway(&router, gateway_name).await;
        let body = json!({
            "rateLimitId": "holds-the-scope",
            "dimensionKeys": dimension_keys,
            "entries": [entry],
        });
        let held = create_rate_limit(&router, &gateway_id, &body).await;
        assert_eq!(
            held.status, 201,
            "case {case:?}: setup expected HTTP 201 from CreateGatewayRateLimit, got {} ({})",
            held.status, held.body
        );
        poll_until_status(&router, &gateway_id, "holds-the-scope", "ACTIVE", case).await;

        // While that limit holds the scope, a second limit over the same keys is refused as a
        // conflict with the one that holds it: "no two rate limits can share the same
        // dimension keys"
        // (`smithy_query(query="doc",shape="CreateGatewayRateLimitRequest",member="dimensionKeys")`).
        let mut second = body.clone();
        second["rateLimitId"] = json!("takes-the-scope");
        let refused = create_rate_limit(&router, &gateway_id, &second).await;
        assert_eq!(
            (refused.status, refused.error_code()),
            (409, "ConflictException".to_string()),
            "case {case:?}: expected the modelled conflict for a limit over the held scope, got \
             {} ({})",
            refused.status,
            refused.body
        );

        let accepted = delete_rate_limit(&router, &gateway_id, "holds-the-scope").await;
        assert_eq!(
            (accepted.status, accepted.text("status")),
            (200, "DELETING".to_string()),
            "case {case:?}: expected HTTP 200 and DELETING from DeleteGatewayRateLimit, got {} \
             ({})",
            accepted.status,
            accepted.body
        );
        poll_until_gone(&router, &gateway_id, "holds-the-scope", case).await;

        // The scope is free: the keys the removed limit held now take a new limit.
        let taken = create_rate_limit(&router, &gateway_id, &second).await;
        assert_eq!(
            taken.status, 201,
            "case {case:?}: expected HTTP 201 for a limit over the freed scope, got {} ({})",
            taken.status, taken.body
        );
        let read = poll_until_status(&router, &gateway_id, "takes-the-scope", "ACTIVE", case).await;
        assert_eq!(
            read.strings("dimensionKeys"),
            dimension_keys
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: the new limit reads back scoped by the freed keys, got {}",
            read.body
        );
        assert_eq!(
            listed_ids(&list_rate_limits(&router, &gateway_id).await.body),
            vec!["takes-the-scope".to_string()],
            "case {case:?}: the gateway lists only the limit that took the freed scope"
        );
    }
}
