// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ListLinkRoutingRules`: enumerating the routing rules one
//! stored link carries.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * the listing reports exactly the rules the store holds for the addressed link,
//!   in a stable total order by `ruleId`, a page of at most `maxResults` at a time,
//!   handing out a `nextToken` only while rules remain and resuming exactly after
//!   the page that issued it;
//! * every listed entry carries what `GetLinkRoutingRule` reports for that same
//!   rule, so the listing and the single-rule reader report one resource;
//! * a rule is addressable only through its own link, from either of that link's two
//!   ends, so another link's rules are never reported and a gateway or link this
//!   store does not hold is the service's one not-found rather than an empty page;
//! * a rule released by `DeleteLinkRoutingRule` leaves the listing once the clock
//!   completes that deletion, while its link's other rules stay;
//! * a `nextToken` belongs to the walk that issued it, so one link's token is not
//!   resumed from by another link's listing.
//!
//! The tables span what the rules generalise over: how many rules the link holds
//! relative to a page (none, fewer, exactly one page, several pages with a short
//! last one), the page size at each published bound of the modelled `maxResults`
//! range (`range(min: 1, max: 10)`) and one past each, which of the link's two ends
//! addresses the call, and each condition member the modelled `RuleCondition`
//! declares (`smithy_query(query="shape", shape="RuleCondition")`).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    RESOURCE_NOT_FOUND_MESSAGE, RULE_STATUS_ACTIVE, RULE_STATUS_CREATION_IN_PROGRESS,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// How long a wait for a clock-owed transition is given before the test gives up.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between two reads of the same records.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The modelled default of this operation's `maxResults`, from
/// `smithy_query(query="operation", operation="ListLinkRoutingRules")`.
const DEFAULT_PAGE: usize = 10;

/// The code a token this walk never issued is answered with: the modelled
/// client-input error the operation declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// One answered call: the status, the wire error code (absent on a success) and the
/// parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    fn message(&self) -> String {
        text(&self.body, "message")
    }
}

/// A requester gateway, a responder gateway and the link between them.
struct Pair {
    requester_id: String,
    responder_id: String,
    link_id: String,
}

/// One condition a rule matches on, as the modelled `RuleCondition` declares its
/// members.
struct Condition {
    name: &'static str,
    stated: fn() -> Value,
}

/// Every condition member the modelled `RuleCondition` declares, plus one row
/// stating two of them together: which condition a rule matches on is what a listed
/// entry has to report back, so the table walks them all.
const CONDITIONS: &[Condition] = &[
    Condition {
        name: "an exact host header",
        stated: || json!({ "hostHeader": "bids.example.com" }),
    },
    Condition {
        name: "a wildcard host header",
        stated: || json!({ "hostHeaderWildcard": "*.example.com" }),
    },
    Condition {
        name: "a path prefix",
        stated: || json!({ "pathPrefix": "/bid" }),
    },
    Condition {
        name: "an exact path",
        stated: || json!({ "pathExact": "/bid/openrtb" }),
    },
    Condition {
        name: "a query string pair",
        stated: || json!({ "queryStringEquals": { "key": "format", "value": "openrtb" } }),
    },
    Condition {
        name: "a query string key that must be present",
        stated: || json!({ "queryStringExists": "debug" }),
    },
    Condition {
        name: "a host header and a path prefix together",
        stated: || json!({ "hostHeader": "bids.example.com", "pathPrefix": "/bid" }),
    },
];

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate ->
/// handle -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one restJson1 request and return what came back.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> Answer {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let body = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        Answer { status, code, body }
    }

    /// A gateway pair with a link between them, all over the wire.
    async fn linked_pair(&self, case: &str, token: &str) -> Pair {
        let requester = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": format!("{token}-requester"),
                })),
            )
            .await;
        assert_eq!(
            requester.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
            requester.status, requester.body
        );
        let requester_id = text(&requester.body, "gatewayId");
        let responder_id = self
            .responder_gateway(case, &format!("{token}-responder"))
            .await;
        let link_id = self.create_link(case, &requester_id, &responder_id).await;
        Pair {
            requester_id,
            responder_id,
            link_id,
        }
    }

    /// Create a responder gateway and return the id the service minted, so a test
    /// can open a second link from the same requester gateway: one gateway pair
    /// holds one link between them, so a second link needs a second peer.
    async fn responder_gateway(&self, case: &str, token: &str) -> String {
        let responder = self
            .send(
                "POST",
                "/responder-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": 443,
                    "protocol": "HTTPS",
                    "domainName": "responder.example.com",
                    "clientToken": token.to_string(),
                })),
            )
            .await;
        assert_eq!(
            responder.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            responder.status, responder.body
        );
        text(&responder.body, "gatewayId")
    }

    /// Open a link between two stored gateways and return the id the service minted.
    async fn create_link(&self, case: &str, requester_id: &str, responder_id: &str) -> String {
        let link = self
            .send(
                "POST",
                &format!("/gateway/{requester_id}/create-link"),
                Some(json!({
                    "peerGatewayId": responder_id,
                    "logSettings": log_settings(),
                })),
            )
            .await;
        assert_eq!(
            link.status, 200,
            "case {case:?}: expected CreateLink to answer 200, got {} {:?}",
            link.status, link.body
        );
        text(&link.body, "linkId")
    }

    /// Create `count` routing rules on one link and return the ids the service
    /// minted, sorted — the order the listing must enumerate them in.
    ///
    /// Each rule takes the next priority, since one link holds one rule per
    /// priority, and cycles through the condition members of [`CONDITIONS`], so a
    /// listing of more than one rule reports more than one kind of condition.
    async fn create_rules(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
        count: usize,
    ) -> Vec<String> {
        let mut ids = Vec::new();
        for n in 0..count {
            let condition = &CONDITIONS[n % CONDITIONS.len()];
            let priority = i64::try_from(n).expect("a priority inside the modelled range") + 1;
            let created = self
                .send(
                    "POST",
                    &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule"),
                    Some(json!({
                        "clientToken": format!("{case}-{link_id}-rule-{n}"),
                        "priority": priority,
                        "conditions": (condition.stated)(),
                    })),
                )
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?} / {:?}: expected CreateLinkRoutingRule #{n} to answer 200, got {} {:?}",
                condition.name,
                created.status, created.body
            );
            ids.push(text(&created.body, "ruleId"));
        }
        ids.sort();
        ids
    }

    async fn get_rule(&self, gateway_id: &str, link_id: &str, rule_id: &str) -> Answer {
        self.send(
            "GET",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            None,
        )
        .await
    }

    /// One page of the listing, as the caller asked for it.
    async fn list(
        &self,
        gateway_id: &str,
        link_id: &str,
        max_results: Option<i64>,
        token: Option<&str>,
    ) -> Answer {
        let mut query: Vec<String> = Vec::new();
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(token) = token {
            query.push(format!("nextToken={token}"));
        }
        let base = format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rules");
        let path = match query.is_empty() {
            true => base,
            false => format!("{base}?{}", query.join("&")),
        };
        self.send("GET", &path, None).await
    }

    /// Walk the whole listing at `max_results` per call, returning every page's
    /// `rules` in the order the pages came back.
    async fn walk(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
        max_results: Option<i64>,
    ) -> Vec<Vec<Value>> {
        let mut pages: Vec<Vec<Value>> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let answered = self
                .list(gateway_id, link_id, max_results, token.as_deref())
                .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected ListLinkRoutingRules to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            pages.push(rules(case, &answered.body));
            token = answered.body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages.len() <= 64,
                "case {case:?}: the walk did not terminate, {} page(s) so far",
                pages.len()
            );
        }
        pages
    }

    /// Walk the listing until every rule it reports is `ACTIVE`, or a generous
    /// deadline passes: settling is a transition the clock owes, so the test waits
    /// for the CONDITION rather than for a span of time. Returns the settled rules in
    /// the order the walk reported them.
    async fn walked_settled(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
        expected: usize,
    ) -> Vec<Value> {
        let give_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            let walked: Vec<Value> = self
                .walk(case, gateway_id, link_id, Some(1))
                .await
                .into_iter()
                .flatten()
                .collect();
            let settled = walked
                .iter()
                .filter(|rule| text(rule, "status") == RULE_STATUS_ACTIVE)
                .count();
            if walked.len() == expected && settled == expected {
                return walked;
            }
            for rule in &walked {
                let status = text(rule, "status");
                assert!(
                    status == RULE_STATUS_ACTIVE || status == RULE_STATUS_CREATION_IN_PROGRESS,
                    "case {case:?}: expected a freshly created rule to report a live status, got {status:?} in {rule:?}"
                );
            }
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected {expected} rule(s) all settled to {RULE_STATUS_ACTIVE:?}, got {walked:?}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }
}

/// The `rules` member of a listing response: the operation always reports the list,
/// so an exhausted enumeration answers an empty one rather than an absent member.
fn rules(case: &str, body: &Value) -> Vec<Value> {
    body["rules"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected rules to be a list, got {body:?}"))
        .clone()
}

/// The `ruleId`s of a page, in the order it reported them.
fn rule_ids(page: &[Value]) -> Vec<String> {
    page.iter().map(|rule| text(rule, "ruleId")).collect()
}

/// Assert `answered` is this service's one not-found.
fn assert_not_found(case: &str, answered: &Answer, request_values: &[&str]) {
    assert_eq!(
        answered.status, 404,
        "case {case:?}: expected HTTP 404, got {} {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {:?}",
        answered.code
    );
    assert_eq!(
        answered.message(),
        RESOURCE_NOT_FOUND_MESSAGE,
        "case {case:?}: expected {RESOURCE_NOT_FOUND_MESSAGE:?}, got {:?}",
        answered.message()
    );
    for value in request_values {
        assert!(
            !answered.message().contains(value),
            "case {case:?}: expected the refusal to name nothing from the request, got {:?}",
            answered.message()
        );
    }
}

/// One row of the pagination table: how many rules the link holds, and the page
/// size the walk asks for.
struct Row {
    name: &'static str,
    stored: usize,
    /// `None` exercises the member's modelled default of 10.
    max_results: Option<i64>,
}

/// The rows differ along what the rule generalises over: the number of stored rules
/// relative to a page (none, fewer, exactly one page, several pages with a short
/// last one) and the page size, at both published bounds of the modelled
/// `maxResults` range.
const ROWS: &[Row] = &[
    Row {
        name: "a link with no rules at all",
        stored: 0,
        max_results: None,
    },
    Row {
        name: "fewer rules than the default page",
        stored: 3,
        max_results: None,
    },
    Row {
        name: "exactly the default page",
        stored: 10,
        max_results: None,
    },
    Row {
        name: "several default pages with a short last one",
        stored: 23,
        max_results: None,
    },
    Row {
        name: "one rule per page, the range's minimum",
        stored: 4,
        max_results: Some(1),
    },
    Row {
        name: "a page size that divides the store exactly",
        stored: 9,
        max_results: Some(3),
    },
    Row {
        name: "the range's maximum over several pages",
        stored: 17,
        max_results: Some(10),
    },
    Row {
        name: "a page larger than the link's rules",
        stored: 2,
        max_results: Some(10),
    },
];

#[tokio::test]
async fn the_listing_reports_every_rule_of_the_link_exactly_once_in_pages() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case, "paging").await;
        let created = fabric
            .create_rules(case, &pair.responder_id, &pair.link_id, row.stored)
            .await;

        let page_size =
            usize::try_from(row.max_results.unwrap_or(DEFAULT_PAGE as i64)).expect("a positive page");
        let pages = fabric
            .walk(case, &pair.responder_id, &pair.link_id, row.max_results)
            .await;

        let expected_pages = row.stored.div_euclid(page_size)
            + usize::from(row.stored.rem_euclid(page_size) != 0 || row.stored == 0);
        assert_eq!(
            pages.len(),
            expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {} rule(s) at {page_size} per page, got {}",
            row.stored,
            pages.len()
        );
        for (index, page) in pages.iter().enumerate() {
            assert!(
                page.len() <= page_size,
                "case {case:?}: page {index} holds {} rule(s), more than the {page_size} asked for: {:?}",
                page.len(),
                rule_ids(page)
            );
        }

        let walked: Vec<String> = pages.iter().flatten().map(|rule| text(rule, "ruleId")).collect();
        assert_eq!(
            walked, created,
            "case {case:?}: expected the walk to report every stored rule exactly once in id order, got {walked:?}"
        );
        let distinct: BTreeSet<&String> = walked.iter().collect();
        assert_eq!(
            distinct.len(),
            walked.len(),
            "case {case:?}: expected no rule to be reported twice, got {walked:?}"
        );
    }
}

#[tokio::test]
async fn a_next_token_is_handed_out_only_while_rules_remain() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case, "token").await;
        let created = fabric
            .create_rules(case, &pair.responder_id, &pair.link_id, row.stored)
            .await;

        let mut seen = 0usize;
        let mut token: Option<String> = None;
        loop {
            let answered = fabric
                .list(
                    &pair.responder_id,
                    &pair.link_id,
                    row.max_results,
                    token.as_deref(),
                )
                .await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected 200, got {} {:?}",
                answered.status, answered.body
            );
            seen += rules(case, &answered.body).len();

            let remaining = created.len().saturating_sub(seen);
            let more_pages_due = remaining > 0;
            assert_eq!(
                answered.body["nextToken"].is_string(),
                more_pages_due,
                "case {case:?}: with {remaining} of {} rule(s) still unread, expected nextToken present={more_pages_due}, got {:?}",
                created.len(),
                answered.body["nextToken"]
            );
            token = answered.body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen,
            created.len(),
            "case {case:?}: expected the walk to read {} rule(s), got {seen}",
            created.len()
        );
    }
}

#[tokio::test]
async fn every_listed_rule_carries_what_the_single_rule_reader_reports() {
    // A rule is addressable from either of its link's two ends, so the listing is
    // driven from both.
    for (case, from_responder) in [("addressed from the responder end", true), ("addressed from the requester end", false)] {
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case, "roundtrip").await;
        let stored = CONDITIONS.len();
        let created = fabric
            .create_rules(case, &pair.responder_id, &pair.link_id, stored)
            .await;
        let addressed_by = match from_responder {
            true => &pair.responder_id,
            false => &pair.requester_id,
        };

        // Compared once every rule has settled, so the listing and the reader are
        // not being asked about a status the clock is still moving.
        let listed = fabric
            .walked_settled(case, addressed_by, &pair.link_id, stored)
            .await;
        assert_eq!(
            rule_ids(&listed),
            created,
            "case {case:?}: expected the listing to report every created rule in id order, got {listed:?}"
        );

        for rule in &listed {
            let rule_id = text(rule, "ruleId");
            let read = fabric
                .get_rule(addressed_by, &pair.link_id, &rule_id)
                .await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetLinkRoutingRule to resolve the listed rule {rule_id:?}, got {} {:?}",
                read.status, read.body
            );
            // Every member the modelled `LinkRoutingRuleSummary` declares, read back
            // out of the same record the single-rule reader reads.
            for member in ["ruleId", "priority", "conditions", "status", "createdAt", "updatedAt"] {
                assert_eq!(
                    rule[member], read.body[member],
                    "case {case:?}: expected the listed {member:?} of {rule_id:?} to match the reader's, got {:?} and {:?}",
                    rule[member], read.body[member]
                );
            }
            // The summary states nothing the model does not declare for it.
            for absent in ["gatewayId", "linkId"] {
                assert!(
                    rule[absent].is_null(),
                    "case {case:?}: expected the summary of {rule_id:?} to carry no {absent:?}, got {:?}",
                    rule[absent]
                );
            }
        }
    }
}

#[tokio::test]
async fn the_listing_reports_only_the_addressed_links_rules_and_changes_nothing() {
    let case = "two links, one gateway pair";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "scoped").await;
    // A second link out of the same requester gateway: its rules are another
    // enumeration, however many the first link holds, and the requester gateway is
    // an end of both links, so one gateway addresses both listings.
    let peer = fabric.responder_gateway(case, "scoped-peer").await;
    let other_link = fabric.create_link(case, &pair.requester_id, &peer).await;
    let hub = pair.requester_id.clone();

    let mine = fabric.create_rules(case, &hub, &pair.link_id, 3).await;
    let theirs = fabric.create_rules(case, &hub, &other_link, 4).await;

    let listed = fabric
        .walk(case, &hub, &pair.link_id, None)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<Value>>();
    assert_eq!(
        rule_ids(&listed),
        mine,
        "case {case:?}: expected only the addressed link's rules, got {listed:?}"
    );
    for id in &theirs {
        assert!(
            !rule_ids(&listed).contains(id),
            "case {case:?}: expected the other link's rule {id:?} to be absent, got {listed:?}"
        );
    }

    let other_listed = fabric
        .walk(case, &hub, &other_link, None)
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<Value>>();
    assert_eq!(
        rule_ids(&other_listed),
        theirs,
        "case {case:?}: expected the other link's own listing to report its rules, got {other_listed:?}"
    );

    // A read changes nothing a later caller sees: once the rules have settled, the
    // same call answers the same ids and the single-rule reader still resolves each
    // of them.
    let settled = fabric
        .walked_settled(case, &hub, &pair.link_id, mine.len())
        .await;
    let again = fabric
        .walked_settled(case, &hub, &pair.link_id, mine.len())
        .await;
    assert_eq!(
        again, settled,
        "case {case:?}: expected a repeated listing to answer identically, got {again:?} after {settled:?}"
    );
    for id in &mine {
        let read = fabric.get_rule(&hub, &pair.link_id, id).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected {id:?} to still be readable after the listing, got {} {:?}",
            read.status, read.body
        );
    }
}

#[tokio::test]
async fn a_released_rule_leaves_the_listing_while_its_siblings_stay() {
    let case = "one rule of three deleted";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "release").await;
    let created = fabric
        .create_rules(case, &pair.responder_id, &pair.link_id, 3)
        .await;
    let released = created[1].clone();
    let remaining: Vec<String> = created
        .iter()
        .filter(|id| **id != released)
        .cloned()
        .collect();

    let deleted = fabric
        .send(
            "DELETE",
            &format!(
                "/responder-gateway/{}/link/{}/routing-rule/{released}",
                pair.responder_id, pair.link_id
            ),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected DeleteLinkRoutingRule to answer 200, got {} {:?}",
        deleted.status, deleted.body
    );

    // The deletion is one the clock completes, so the test waits for the CONDITION:
    // the listing stops reporting that rule, and reports every other rule of the
    // link throughout.
    let give_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let listed = fabric
            .walk(case, &pair.responder_id, &pair.link_id, None)
            .await
            .into_iter()
            .flatten()
            .collect::<Vec<Value>>();
        let ids = rule_ids(&listed);
        for id in &remaining {
            assert!(
                ids.contains(id),
                "case {case:?}: expected the link's other rule {id:?} to stay listed, got {ids:?}"
            );
        }
        if !ids.contains(&released) {
            assert_eq!(
                ids, remaining,
                "case {case:?}: expected exactly the surviving rules once the deletion completed, got {ids:?}"
            );
            break;
        }
        assert!(
            Instant::now() < give_up_at,
            "case {case:?}: expected {released:?} to leave the listing, got {ids:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

#[tokio::test]
async fn a_gateway_or_link_the_store_does_not_hold_is_refused_with_the_one_not_found() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("not found", "missing").await;
    // A second pair, so a stored gateway that is no end of the addressed link is
    // among the rows rather than only ids nothing minted.
    let stranger = fabric.linked_pair("not found", "stranger").await;
    fabric
        .create_rules("not found", &pair.responder_id, &pair.link_id, 2)
        .await;

    for (case, gateway_id, link_id) in [
        (
            "a gateway id nothing minted",
            "rtb-gw-00000000000000000000".to_string(),
            pair.link_id.clone(),
        ),
        (
            "a link id nothing minted",
            pair.responder_id.clone(),
            "link-00000000000000000000".to_string(),
        ),
        (
            "a stored gateway that is no end of the link",
            stranger.responder_id.clone(),
            pair.link_id.clone(),
        ),
        (
            "a stored link of another gateway pair",
            pair.responder_id.clone(),
            stranger.link_id.clone(),
        ),
    ] {
        let answered = fabric.list(&gateway_id, &link_id, None, None).await;
        assert_not_found(case, &answered, &[gateway_id.as_str(), link_id.as_str()]);
        assert!(
            answered.body["rules"].is_null(),
            "case {case:?}: expected no page at all, got {:?}",
            answered.body
        );
    }
}

#[tokio::test]
async fn a_page_size_past_a_published_bound_is_refused() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("bounds", "bounds").await;
    fabric
        .create_rules("bounds", &pair.responder_id, &pair.link_id, 2)
        .await;

    // The published bounds of the modelled `maxResults` range are 1 and 10; one
    // past each is refused before the handler runs, and each bound itself is
    // accepted.
    for (case, max_results, expected) in [
        ("one below the minimum", 0, 400),
        ("the minimum", 1, 200),
        ("the maximum", 10, 200),
        ("one above the maximum", 11, 400),
    ] {
        let answered = fabric
            .list(&pair.responder_id, &pair.link_id, Some(max_results), None)
            .await;
        assert_eq!(
            answered.status, expected,
            "case {case:?}: expected maxResults={max_results} to answer {expected}, got {} {:?}",
            answered.status, answered.body
        );
    }
}

#[tokio::test]
async fn one_links_pagination_token_is_not_resumed_from_by_another_listing() {
    let case = "a token replayed on another enumeration";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "cursor").await;
    let peer = fabric.responder_gateway(case, "cursor-peer").await;
    let other_link = fabric.create_link(case, &pair.requester_id, &peer).await;
    let hub = pair.requester_id.clone();
    let mine = fabric.create_rules(case, &hub, &pair.link_id, 4).await;
    let theirs = fabric.create_rules(case, &hub, &other_link, 4).await;

    let first = fabric.list(&hub, &pair.link_id, Some(1), None).await;
    assert_eq!(
        first.status, 200,
        "case {case:?}: expected the first page to answer 200, got {} {:?}",
        first.status, first.body
    );
    let token = first.body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a token while rules remain, got {:?}", first.body)
        })
        .to_string();

    // The issuing walk resumes from its own token, strictly after the page it
    // answered.
    let resumed = fabric
        .list(&hub, &pair.link_id, Some(1), Some(&token))
        .await;
    assert_eq!(
        resumed.status, 200,
        "case {case:?}: expected the issuing walk to resume from its own token, got {} {:?}",
        resumed.status, resumed.body
    );
    assert_eq!(
        rule_ids(&rules(case, &resumed.body)),
        vec![mine[1].clone()],
        "case {case:?}: expected the next page to resume after the first rule, got {:?}",
        resumed.body
    );

    // Another link's enumeration never issued it, so it is refused rather than read
    // as an index into that link's rules.
    let elsewhere = fabric
        .list(&hub, &other_link, Some(1), Some(&token))
        .await;
    assert_eq!(
        elsewhere.status, 400,
        "case {case:?}: expected another link's listing to refuse the token, got {} {:?}",
        elsewhere.status, elsewhere.body
    );
    assert_eq!(
        elsewhere.code, VALIDATION_EXCEPTION,
        "case {case:?}: expected the modelled client-input error, got {:?}",
        elsewhere.code
    );
    assert!(
        !theirs.is_empty(),
        "case {case:?}: the other link must hold rules for the refusal to be about the token"
    );
}
