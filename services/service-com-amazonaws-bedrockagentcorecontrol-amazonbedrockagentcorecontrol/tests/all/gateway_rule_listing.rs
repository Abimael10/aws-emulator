// SPDX-License-Identifier: CC0-1.0

//! `ListGatewayRules` end to end over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="ListGatewayRules")`): the listing reports the
//! rules the gateway the path names holds — "Lists all rules for a gateway" — each rendered
//! off the same stored record `GetGatewayRule` answers from, in one page per
//! `maxResults` (`GatewayRuleMaxResults`, `@range 1..100`, the page's own documented
//! maximum when the caller names none) with a `nextToken` that resumes exactly where the
//! page ended and is absent on the last page. A rule is addressed only THROUGH its gateway,
//! so a listing for a gateway no account holds is the modelled 404 rather than an empty
//! page, and a `nextToken` the service did not issue is the modelled 400.
//!
//! The rows differ along what the rule generalises over: how many rules the gateway holds
//! (none, one, several), the page size (the published lower bound of `maxResults`, a size
//! that divides the set, one that leaves a remainder, one above the set's size, and none at
//! all) and the token the page is resumed with.

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

/// How long a poll loop keeps looking for a clock-owed transition before giving up.
/// Generous on purpose: the loop waits for the CONDITION, and the span this service gives
/// the gateway family is far shorter than this, on any host.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll loop pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// An identifier inside `GatewayIdentifier`'s pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$` that no gateway of this account holds.
const UNHELD_GATEWAY: &str = "nosuchgateway-abcdefghij";

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

/// Create a gateway and hand back its `gatewayId` once the gateway reports `READY` — a
/// rule write is refused until the gateway is routable.
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

/// One rule at `priority`, routed to `target`.
async fn create_rule(
    router: &MultiServiceRouter,
    gateway_id: &str,
    priority: i64,
    target: &str,
) -> String {
    let created = send(
        router,
        "POST",
        &format!("/gateways/{gateway_id}/rules"),
        Some(&json!({
            "priority": priority,
            "conditions": [{"matchPaths": {"anyOf": [format!("/{target}/*")]}}],
            "actions": [{"routeToTarget": {"staticRoute": {"targetName": target}}}],
            "description": format!("route to {target}"),
        })),
    )
    .await;
    assert_eq!(
        created.status, 202,
        "setup: expected HTTP 202 for the rule at priority {priority}, got {} ({})",
        created.status, created.body
    );
    text(&created.body, "ruleId")
}

/// One page of the listing, with an optional page size and resume token.
async fn list_page(
    router: &MultiServiceRouter,
    gateway_id: &str,
    max_results: Option<i32>,
    next_token: Option<&str>,
) -> Answer {
    let mut query: Vec<String> = Vec::new();
    if let Some(max_results) = max_results {
        query.push(format!("maxResults={max_results}"));
    }
    if let Some(next_token) = next_token {
        query.push(format!("nextToken={next_token}"));
    }
    let uri = if query.is_empty() {
        format!("/gateways/{gateway_id}/rules")
    } else {
        format!("/gateways/{gateway_id}/rules?{}", query.join("&"))
    };
    send(router, "GET", &uri, None).await
}

/// The `ruleId`s one page reports, in the order the page carries them.
fn rule_ids(page: &Value) -> Vec<String> {
    page.get("gatewayRules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|rule| text(rule, "ruleId"))
        .collect()
}

/// Every rule of the gateway, walked page by page at `page_size`, with the number of pages
/// the walk took.
async fn walk_pages(
    router: &MultiServiceRouter,
    gateway_id: &str,
    page_size: Option<i32>,
) -> (Vec<String>, usize) {
    let mut seen: Vec<String> = Vec::new();
    let mut pages = 0_usize;
    let mut token: Option<String> = None;
    loop {
        let page = list_page(router, gateway_id, page_size, token.as_deref()).await;
        assert_eq!(
            page.status, 200,
            "page {pages} at size {page_size:?}: expected HTTP 200, got {} ({})",
            page.status, page.body
        );
        pages += 1;
        if let Some(size) = page_size {
            let carried = rule_ids(&page.body).len();
            assert!(
                carried <= usize::try_from(size).expect("a page size fits a usize"),
                "page {pages} at size {page_size:?}: a page holds at most maxResults rules, \
                 got {carried}"
            );
        }
        seen.extend(rule_ids(&page.body));
        match page.body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => return (seen, pages),
        }
        assert!(
            pages < 100,
            "the listing never ran out of pages at size {page_size:?}, after {pages}"
        );
    }
}

/// Every page size the walk is driven at: none at all, the published lower bound of
/// `maxResults` (`@range 1..100`), a size that divides the set, one that leaves a
/// remainder, one at the set's size and one above it.
const PAGE_SIZES: [(Option<i32>, usize); 6] = [
    (None, 1),
    (Some(1), 6),
    (Some(2), 3),
    (Some(4), 2),
    (Some(6), 1),
    (Some(100), 1),
];

/// The listing reports every rule the gateway holds, exactly once, however it is paged, and
/// each row is the rule the single-rule read reports.
#[tokio::test]
async fn the_listing_pages_over_every_rule_the_gateway_holds() {
    let router = router();
    let gateway_id = gateway(&router, "listgatewayrulespaging").await;

    // Six rules at distinct priorities: one gateway holds at most one rule per priority.
    let mut written: Vec<String> = Vec::new();
    for (priority, target) in [
        (1_i64, "orders"),
        (10, "reports"),
        (100, "billing"),
        (1_000, "search"),
        (10_000, "admin"),
        (1_000_000, "fallback"),
    ] {
        written.push(create_rule(&router, &gateway_id, priority, target).await);
    }
    written.sort();

    for (page_size, expected_pages) in PAGE_SIZES {
        let (mut seen, pages) = walk_pages(&router, &gateway_id, page_size).await;
        assert_eq!(
            pages, expected_pages,
            "case {page_size:?}: expected {expected_pages} page(s), took {pages}"
        );
        let paged = seen.clone();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            paged.len(),
            "case {page_size:?}: no rule is reported twice, got {paged:?}"
        );
        assert_eq!(
            seen, written,
            "case {page_size:?}: the listing reports every rule the gateway holds, got {seen:?}"
        );
    }

    // Each row is the rule the single-rule read reports, off the same record.
    let whole = list_page(&router, &gateway_id, None, None).await;
    for row in whole
        .body
        .get("gatewayRules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let rule_id = text(&row, "ruleId");
        let read = send(
            &router,
            "GET",
            &format!("/gateways/{gateway_id}/rules/{rule_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "the listed rule {rule_id} must read back, got {} ({})",
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
                row.get(member),
                read.body.get(member),
                "rule {rule_id}: the listing reports the {member} the read reports, got {row}"
            );
        }
    }
}

/// A gateway that holds no rules is an empty page, not a 404, and it ends the listing.
#[tokio::test]
async fn a_gateway_with_no_rules_is_an_empty_page() {
    let router = router();
    let gateway_id = gateway(&router, "listgatewayrulesempty").await;

    let page = list_page(&router, &gateway_id, None, None).await;
    assert_eq!(
        page.status, 200,
        "a gateway that holds no rules still answers the listing, got {} ({})",
        page.status, page.body
    );
    assert_eq!(
        rule_ids(&page.body),
        Vec::<String>::new(),
        "a gateway that holds no rules reports none, got {}",
        page.body
    );
    assert!(
        page.body.get("nextToken").is_none() || page.body["nextToken"].is_null(),
        "an empty listing has nothing to resume, got {}",
        page.body
    );
}

/// A rule is addressed only through its gateway: a listing for a gateway no account holds
/// is the modelled 404, and a token this service did not issue is the modelled 400.
#[tokio::test]
async fn a_listing_nothing_holds_and_a_token_nothing_issued_are_refused() {
    let router = router();
    let gateway_id = gateway(&router, "listgatewayrulesrefusals").await;
    create_rule(&router, &gateway_id, 5, "orders").await;

    let unheld = list_page(&router, UNHELD_GATEWAY, None, None).await;
    assert_eq!(
        (unheld.status, error_code(&unheld.body)),
        (404, "ResourceNotFoundException".to_string()),
        "a listing for a gateway nothing holds is the modelled 404, got {} ({})",
        unheld.status,
        unheld.body
    );

    for token in ["zzzz", "not-a-token"] {
        let refused = list_page(&router, &gateway_id, None, Some(token)).await;
        assert_eq!(
            (refused.status, error_code(&refused.body)),
            (400, "ValidationException".to_string()),
            "case {token:?}: a token this service did not issue is the modelled 400, got {} ({})",
            refused.status,
            refused.body
        );
    }
}
