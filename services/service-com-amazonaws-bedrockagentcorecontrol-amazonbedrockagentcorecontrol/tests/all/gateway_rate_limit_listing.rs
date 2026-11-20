// SPDX-License-Identifier: CC0-1.0

//! `ListGatewayRateLimits`, end to end over HTTP, against the limits the writes of a
//! gateway's rate-limit set stored.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint walk and the handlers are all exercised against the one shared engine.
//!
//! THE RULE UNDER TEST, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation", operation="ListGatewayRateLimits,\
//! GetGatewayRateLimit,CreateGatewayRateLimit")`): the listing reports the rate limits
//! PERSISTED on the gateway the request names — "lists all rate limits for a gateway.
//! Results are paginated. Use the nextToken parameter to retrieve additional results" —
//! one `GatewayRateLimitDetail` per stored limit, carrying what the write stored and the
//! status the limit reports, so every row equals what `GetGatewayRateLimit` answers for
//! that same limit. A page holds at most `maxResults` rows and hands back a `nextToken`
//! while rows are left over; a gateway this service does not hold is the modelled 404
//! rather than an empty page.
//!
//! The rows differ along what the rule generalises over: how many limits a gateway holds,
//! how large a page is asked for (one, some and all of them), which rate metric each limit
//! configures, whether a limit carries a description, and whether the gateway holds any
//! limit at all.

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

/// How long a test waits for a clock-owed transition before failing. Far longer than the
/// span `src/state.rs` gives the gateway family, so a loaded host cannot fail a test: each
/// loop waits for the CONDITION, and the deadline only catches a transition that never
/// comes.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop waits between two looks at the same resource.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The terminal statuses a gateway and a rate limit settle into
/// (`smithy_query(query="shape", shape="GatewayStatus,GatewayRateLimitStatus")`).
const STATUS_READY: &str = "READY";
const STATUS_ACTIVE: &str = "ACTIVE";

/// The gateway family's whole not-found sentence, which every gateway lookup answers with.
const GATEWAY_NOT_FOUND: &str = "Failed to retrieve gateway because it doesn't exist. Retry the \
                                 request with a different resource identifier.";

/// Dimension keys from `DimensionKey`'s own published catalogue
/// (`smithy_query(query="shape", shape="DimensionKey")`), one per limit a test stores, since
/// no two limits of a gateway may share their dimension keys.
const DIMENSION_KEYS: [&str; 3] = ["targetName", "toolName", "qualifiedModelId"];

/// The rate metrics `LimitEntry` configures, one per stored limit below
/// (`smithy_query(query="shape", shape="LimitEntry")`).
const RATE_METRICS: [&str; 3] = ["requests", "tokens", "connections"];

/// The ids the stored limits carry, inside `GatewayRateLimitId`'s pattern
/// `^[a-zA-Z0-9][a-zA-Z0-9\-_\.]{0,62}[a-zA-Z0-9]$`. Ascending, which is the order a page
/// walks the set in.
const RATE_LIMIT_IDS: [&str; 3] = ["limit-a", "limit-b", "limit-c"];

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

/// The `rateLimits` rows of one listing answer.
fn rows(body: &Value) -> Vec<Value> {
    body.get("rateLimits")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Create a gateway and hand back its `gatewayId`, waited out to its terminal status.
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

/// Store `count` rate limits on `gateway`, each scoped by its own dimension key and
/// configuring its own rate metric, and wait each one out to its terminal status so a later
/// read of it cannot race the transition the clock owes it.
async fn stored_rate_limits(router: &MultiServiceRouter, gateway: &str, count: usize) {
    for index in 0..count {
        let key = DIMENSION_KEYS[index];
        let metric = RATE_METRICS[index];
        let mut entry = json!({"dimensions": {key: "orders"}});
        entry[metric] = json!([{"rate": 25.0, "period": "second"}]);
        let mut body = json!({
            "rateLimitId": RATE_LIMIT_IDS[index],
            "dimensionKeys": [key],
            "entries": [entry],
        });
        // Every other limit carries a description, so the rows differ in what they hold.
        if index % 2 == 0 {
            body["description"] = json!(format!("limit {index}"));
        }
        let created = send(
            router,
            "POST",
            &format!("/gateways/{gateway}/rate-limits"),
            Some(&body),
        )
        .await;
        assert_eq!(
            created.status, 201,
            "setup {:?}: expected HTTP 201 from CreateGatewayRateLimit, got {} ({})",
            RATE_LIMIT_IDS[index], created.status, created.body
        );
    }

    for rate_limit in RATE_LIMIT_IDS.iter().take(count) {
        let started = Instant::now();
        loop {
            let read = send(
                router,
                "GET",
                &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
                None,
            )
            .await;
            if text(&read.body, "status") == STATUS_ACTIVE {
                break;
            }
            assert!(
                started.elapsed() < SETTLE_DEADLINE,
                "setup {rate_limit:?}: the rate limit never reached {STATUS_ACTIVE}, last answer \
                 {} ({})",
                read.status,
                read.body
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// Every stored rate limit is listed, and each row is exactly what a read of that one limit
/// answers.
#[tokio::test]
async fn the_listing_reports_every_stored_rate_limit() {
    let router = router();
    let gateway = settled_gateway(&router, "ratelimitlisting").await;
    stored_rate_limits(&router, &gateway, RATE_LIMIT_IDS.len()).await;

    let listed = send(
        &router,
        "GET",
        &format!("/gateways/{gateway}/rate-limits"),
        None,
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case \"every stored limit\": expected HTTP 200, got {} ({})",
        listed.status, listed.body
    );
    let rows = rows(&listed.body);
    assert_eq!(
        rows.len(),
        RATE_LIMIT_IDS.len(),
        "case \"every stored limit\": expected {} rows, got {}",
        RATE_LIMIT_IDS.len(),
        listed.body
    );
    assert!(
        listed.body.get("nextToken").is_none(),
        "case \"every stored limit\": a page holding every row carries no nextToken, got {}",
        listed.body
    );

    for (index, row) in rows.iter().enumerate() {
        // The set is walked in ascending id order, which is the one order two pages of it
        // can be read in without a row being missed or repeated.
        let rate_limit = RATE_LIMIT_IDS[index];
        assert_eq!(
            text(row, "rateLimitId"),
            rate_limit,
            "case {rate_limit:?}: expected the rows in ascending id order, got {}",
            listed.body
        );
        let read = send(
            &router,
            "GET",
            &format!("/gateways/{gateway}/rate-limits/{rate_limit}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {rate_limit:?}: expected HTTP 200 from GetGatewayRateLimit, got {} ({})",
            read.status, read.body
        );
        assert_eq!(
            row, &read.body,
            "case {rate_limit:?}: a listed row is what a read of that limit answers, got {row} \
             and {}",
            read.body
        );
        assert_eq!(
            text(row, "gatewayIdentifier"),
            gateway,
            "case {rate_limit:?}: every row names the gateway it was addressed through, got {row}"
        );
    }
}

/// A page holds at most `maxResults` rows, and the `nextToken` it hands back resumes where
/// it ended — so following the tokens reports every stored limit exactly once, whatever the
/// page size.
#[tokio::test]
async fn the_pages_of_a_listing_report_every_limit_once() {
    let router = router();
    let gateway = settled_gateway(&router, "ratelimitpaging").await;
    stored_rate_limits(&router, &gateway, RATE_LIMIT_IDS.len()).await;

    // (case, maxResults)
    let page_sizes: [(&str, usize); 3] = [
        ("one row per page", 1),
        ("two rows per page", 2),
        ("every row in one page", RATE_LIMIT_IDS.len()),
    ];

    for (case, page_size) in page_sizes {
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let uri = match &token {
                None => format!("/gateways/{gateway}/rate-limits?maxResults={page_size}"),
                Some(token) => format!(
                    "/gateways/{gateway}/rate-limits?maxResults={page_size}&nextToken={token}"
                ),
            };
            let page = send(&router, "GET", &uri, None).await;
            assert_eq!(
                page.status, 200,
                "case {case:?}: expected HTTP 200, got {} ({})",
                page.status, page.body
            );
            let rows = rows(&page.body);
            assert!(
                !rows.is_empty() && rows.len() <= page_size,
                "case {case:?}: a page holds between one and {page_size} rows, got {}",
                page.body
            );
            for row in rows {
                seen.push(text(&row, "rateLimitId"));
            }
            token = page
                .body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen,
            RATE_LIMIT_IDS
                .iter()
                .map(|id| (*id).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: following the tokens reports every stored limit once, in id order, \
             got {seen:?}"
        );
    }
}

/// What the listing refuses, and what a gateway holding no limit answers.
#[tokio::test]
async fn the_listing_refuses_what_it_cannot_report() {
    let router = router();
    let gateway = settled_gateway(&router, "ratelimitrefusals").await;

    // A gateway that holds no rate limit is an empty page, not a refusal: the gateway is
    // there and holds nothing.
    let empty = send(
        &router,
        "GET",
        &format!("/gateways/{gateway}/rate-limits"),
        None,
    )
    .await;
    assert_eq!(
        (empty.status, rows(&empty.body).len()),
        (200, 0),
        "case \"a gateway holding no limit\": expected HTTP 200 and no rows, got {} ({})",
        empty.status,
        empty.body
    );

    // A gateway this service does not hold is the modelled 404 in the gateway family's own
    // sentence, never an empty page. The identifier is inside `GatewayIdentifier`'s
    // pattern, so this is the handler's refusal and not the boundary's.
    let missing = send(&router, "GET", "/gateways/no-such-gateway-0123456789/rate-limits", None)
        .await;
    assert_eq!(
        (missing.status, error_code(&missing.body).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"an unknown gateway\": expected HTTP 404 ResourceNotFoundException, got {} {} ({})",
        missing.status,
        error_code(&missing.body),
        missing.body
    );
    assert_eq!(
        text(&missing.body, "message"),
        GATEWAY_NOT_FOUND,
        "case \"an unknown gateway\": expected the gateway family's not-found sentence, got {}",
        text(&missing.body, "message")
    );

    // A token this service never issued is refused rather than read as a position: a page
    // resumed from nowhere would silently skip rows.
    let bad_token = send(
        &router,
        "GET",
        &format!("/gateways/{gateway}/rate-limits?nextToken=notatokenthisserviceissued"),
        None,
    )
    .await;
    assert_eq!(
        (bad_token.status, error_code(&bad_token.body).as_str()),
        (400, "ValidationException"),
        "case \"a token nothing issued\": expected HTTP 400 ValidationException, got {} {} ({})",
        bad_token.status,
        error_code(&bad_token.body),
        bad_token.body
    );
}
