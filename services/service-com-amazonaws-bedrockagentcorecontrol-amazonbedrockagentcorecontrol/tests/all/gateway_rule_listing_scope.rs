// SPDX-License-Identifier: CC0-1.0

//! What `ListGatewayRules` reports, and what it does NOT: the listing's scope, its order
//! and its purity, end to end over HTTP.
//!
//! Each test drives the service the way the router does: an `http::Request` through
//! [`MultiServiceRouter::dispatch`], so the wire protocol (`restJson1`), the boundary
//! constraint validation and the handlers are all exercised against the one shared engine.
//!
//! The rule under test, read off the Smithy model rather than off any one recorded request
//! (`smithy_query(query="operation",operation="ListGatewayRules,GetGatewayRule,UpdateGatewayRule")`):
//! a `@paginated`, `@readonly` listing of a child resource reports exactly the children the
//! parent named by the path holds — "Lists all rules for a gateway" — in ascending stored-key
//! (`ruleId`) order, each page's `nextToken` resuming strictly after the child that page
//! ended on, every entry rendered off the record the single-child read answers from
//! (including a change a later `PATCH` made), and the call leaves the store exactly as it
//! found it, so a second identical listing reports the same thing.
//!
//! [`crate::gateway_rule_listing`] covers how the set is split into pages; the rows here
//! differ along the other axes the rule generalises over: how many rules each gateway holds
//! (none, one, several), which gateway of several the path names, whether a rule was changed
//! after it was created, and how often the listing is read.

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

/// The status a settled rule reports (`GatewayRuleStatus`), which a change to a rule is
/// refused until it holds.
const ACTIVE: &str = "ACTIVE";

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

/// The single-rule read of one rule.
async fn read_rule(router: &MultiServiceRouter, gateway_id: &str, rule_id: &str) -> Answer {
    send(
        router,
        "GET",
        &format!("/gateways/{gateway_id}/rules/{rule_id}"),
        None,
    )
    .await
}

/// Look at one rule until the transition the clock owes it has come due, and hand back the
/// read that saw it settled.
async fn settled_rule(router: &MultiServiceRouter, gateway_id: &str, rule_id: &str) -> Answer {
    let started = Instant::now();
    loop {
        let read = read_rule(router, gateway_id, rule_id).await;
        if text(&read.body, "status") == ACTIVE {
            return read;
        }
        assert!(
            started.elapsed() <= POLL_DEADLINE,
            "rule {rule_id}: never reported {ACTIVE}, last read {}",
            read.body
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
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

/// The rows one page carries.
fn rows(page: &Value) -> Vec<Value> {
    page.get("gatewayRules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The `ruleId`s one page reports, in the order the page carries them.
fn rule_ids(page: &Value) -> Vec<String> {
    rows(page)
        .iter()
        .map(|rule| text(rule, "ruleId"))
        .collect()
}

/// Every page of the listing at `page_size`, in the order the walk read them.
async fn pages(
    router: &MultiServiceRouter,
    gateway_id: &str,
    page_size: Option<i32>,
) -> Vec<Value> {
    let mut read: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    loop {
        let page = list_page(router, gateway_id, page_size, token.as_deref()).await;
        assert_eq!(
            page.status, 200,
            "page {} at size {page_size:?}: expected HTTP 200, got {} ({})",
            read.len(),
            page.status,
            page.body
        );
        token = page
            .body
            .get("nextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        read.push(page.body);
        if token.is_none() {
            return read;
        }
        assert!(
            read.len() < 100,
            "the listing never ran out of pages at size {page_size:?}, after {}",
            read.len()
        );
    }
}

/// The page sizes each ordering row is walked at: none at all, the published lower bound of
/// `maxResults` (`@range 1..100`), a size that divides the set, one that leaves a remainder,
/// and one above the set's size.
const PAGE_SIZES: [Option<i32>; 5] = [None, Some(1), Some(2), Some(3), Some(100)];

/// The priorities the ordering row writes its rules at, inside `priority`'s modelled
/// `@range 1..1000000`: a rule's stored key is not its priority, so writing them out of
/// priority order is what shows the listing orders by `ruleId`.
const ORDERING_PRIORITIES: [(i64, &str); 5] = [
    (900_000, "alpha"),
    (7, "bravo"),
    (1_000_000, "charlie"),
    (1, "delta"),
    (512, "echo"),
];

/// However the set is paged, the pages concatenate into every rule the gateway holds in
/// ascending `ruleId` order, and each page's `nextToken` resumes strictly after the rule
/// that page ended on.
#[tokio::test]
async fn the_pages_are_ascending_by_rule_id_and_resume_after_the_rule_they_ended_on() {
    let router = router();
    let gateway_id = gateway(&router, "listgatewayrulesorder").await;

    let mut written: Vec<String> = Vec::new();
    for (priority, target) in ORDERING_PRIORITIES {
        written.push(create_rule(&router, &gateway_id, priority, target).await);
    }
    written.sort();

    for page_size in PAGE_SIZES {
        let walked = pages(&router, &gateway_id, page_size).await;
        let mut seen: Vec<String> = Vec::new();
        for (index, page) in walked.iter().enumerate() {
            let ids = rule_ids(page);
            let mut ascending = ids.clone();
            ascending.sort();
            assert_eq!(
                ids, ascending,
                "case {page_size:?}, page {index}: a page is ascending by ruleId, got {ids:?}"
            );
            if let (Some(last), Some(first)) = (seen.last(), ids.first()) {
                assert!(
                    first > last,
                    "case {page_size:?}, page {index}: the token resumes strictly after \
                     {last:?}, got {first:?}"
                );
            }
            seen.extend(ids);
        }
        assert_eq!(
            seen, written,
            "case {page_size:?}: the listing reports every rule the gateway holds once, in \
             ascending ruleId order, got {seen:?}"
        );
    }
}

/// How many rules each gateway of the row holds: none, one, several.
const GATEWAY_RULE_COUNTS: [(&str, usize); 3] = [
    ("listgatewayrulesscopenone", 0),
    ("listgatewayrulesscopeone", 1),
    ("listgatewayrulesscopemany", 3),
];

/// A rule is addressed only THROUGH its gateway: the listing reports exactly the rules the
/// gateway the path names holds, and never one of a sibling gateway.
#[tokio::test]
async fn the_listing_reports_only_the_rules_the_gateway_in_the_path_holds() {
    let router = router();

    let mut held: Vec<(String, Vec<String>)> = Vec::new();
    for (name, count) in GATEWAY_RULE_COUNTS {
        let gateway_id = gateway(&router, name).await;
        let mut ids: Vec<String> = Vec::new();
        for index in 0..count {
            let priority = i64::try_from(index).expect("a small index fits an i64") + 1;
            let target = format!("{name}{index}");
            ids.push(create_rule(&router, &gateway_id, priority, &target).await);
        }
        ids.sort();
        held.push((gateway_id, ids));
    }

    for (gateway_id, ids) in &held {
        let page = list_page(&router, gateway_id, None, None).await;
        assert_eq!(
            page.status, 200,
            "gateway {gateway_id}: expected HTTP 200 from the listing, got {} ({})",
            page.status, page.body
        );
        assert_eq!(
            &rule_ids(&page.body),
            ids,
            "gateway {gateway_id}: the listing reports exactly the {} rule(s) it holds, got {}",
            ids.len(),
            page.body
        );
        let gateway_arn = text(
            &send(&router, "GET", &format!("/gateways/{gateway_id}/"), None)
                .await
                .body,
            "gatewayArn",
        );
        for row in rows(&page.body) {
            assert_eq!(
                text(&row, "gatewayArn"),
                gateway_arn,
                "gateway {gateway_id}: every reported rule carries that gateway's ARN, got {row}"
            );
        }
        let reported = rule_ids(&page.body);
        for (other, other_ids) in &held {
            if other == gateway_id {
                continue;
            }
            for leaked in other_ids {
                assert!(
                    !reported.contains(leaked),
                    "gateway {gateway_id}: reported {leaked:?}, which gateway {other} holds, \
                     got {}",
                    page.body
                );
            }
        }
    }
}

/// The members a rule reports, as `GetGatewayRule` and the listing's `GatewayRuleDetail`
/// both declare them (`smithy_query operation=GetGatewayRule`).
const RULE_MEMBERS: [&str; 9] = [
    "ruleId",
    "gatewayArn",
    "priority",
    "conditions",
    "actions",
    "description",
    "createdAt",
    "updatedAt",
    "status",
];

/// A rule a later `PATCH` changed is reported by the listing exactly as the single-rule read
/// reports it — the listing renders the stored record, never the one the create submitted.
#[tokio::test]
async fn a_changed_rule_is_reported_as_the_single_rule_read_reports_it() {
    let router = router();
    let gateway_id = gateway(&router, "listgatewayruleschanged").await;

    let changed = create_rule(&router, &gateway_id, 42, "orders").await;
    let untouched = create_rule(&router, &gateway_id, 43, "reports").await;
    settled_rule(&router, &gateway_id, &changed).await;

    let updated = send(
        &router,
        "PATCH",
        &format!("/gateways/{gateway_id}/rules/{changed}"),
        Some(&json!({"priority": 77, "description": "route to invoices"})),
    )
    .await;
    assert_eq!(
        updated.status, 202,
        "expected HTTP 202 from UpdateGatewayRule, got {} ({})",
        updated.status, updated.body
    );
    let read = settled_rule(&router, &gateway_id, &changed).await;
    assert_eq!(
        read.body.get("priority"),
        Some(&json!(77)),
        "the read reports the priority the change submitted, got {}",
        read.body
    );

    let page = list_page(&router, &gateway_id, None, None).await;
    let row = rows(&page.body)
        .into_iter()
        .find(|row| text(row, "ruleId") == changed)
        .unwrap_or_else(|| panic!("the changed rule is still listed, got {}", page.body));
    for member in RULE_MEMBERS {
        assert_eq!(
            row.get(member),
            read.body.get(member),
            "rule {changed}: the listing reports the {member} the read reports, got {row}"
        );
    }
    assert_eq!(
        row.get("description"),
        Some(&json!("route to invoices")),
        "rule {changed}: the listing reports the description the change submitted, got {row}"
    );

    let sibling = rows(&page.body)
        .into_iter()
        .find(|row| text(row, "ruleId") == untouched)
        .unwrap_or_else(|| panic!("the untouched rule is still listed, got {}", page.body));
    assert_eq!(
        sibling.get("priority"),
        Some(&json!(43)),
        "rule {untouched}: a change to a sibling leaves it as it was, got {sibling}"
    );
}

/// The listing is a report: reading it twice over an unchanged store reports the same thing,
/// at every page size, and leaves the rules the single-rule read reports untouched.
#[tokio::test]
async fn reading_the_listing_twice_reports_the_same_rules() {
    let router = router();
    let gateway_id = gateway(&router, "listgatewayrulesrepeat").await;

    let mut written: Vec<String> = Vec::new();
    for (priority, target) in [(3_i64, "orders"), (33, "reports"), (333, "billing")] {
        written.push(create_rule(&router, &gateway_id, priority, target).await);
    }
    // Settled first, so the only difference a second read could report is one this call
    // made rather than one the clock owed.
    for rule_id in &written {
        settled_rule(&router, &gateway_id, rule_id).await;
    }

    for page_size in [None, Some(1), Some(2), Some(100)] {
        let first = pages(&router, &gateway_id, page_size).await;
        let again = pages(&router, &gateway_id, page_size).await;
        assert_eq!(
            first, again,
            "case {page_size:?}: a second identical listing reports the same pages, got {again:?}"
        );
    }

    for rule_id in &written {
        let read = read_rule(&router, &gateway_id, rule_id).await;
        assert_eq!(
            (read.status, text(&read.body, "status")),
            (200, ACTIVE.to_string()),
            "rule {rule_id}: the listings left it as they found it, got {} ({})",
            read.status,
            read.body
        );
    }
}
