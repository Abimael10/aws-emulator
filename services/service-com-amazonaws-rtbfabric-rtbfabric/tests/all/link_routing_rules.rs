// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the link routing-rule surface: `CreateLinkRoutingRule` and
//! `GetLinkRoutingRule`, driven over the wire through the router the production
//! emulator uses.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * a routing rule is a record on ONE stored link, addressed through that link from
//!   either of its two ends, so the read reports exactly what the create stored and a
//!   rule is never readable through another link;
//! * a rule is created `CREATION_IN_PROGRESS` and the clock settles it to `ACTIVE`;
//! * a rule cannot outlive the link it routes for, so deleting the link takes its
//!   rules with it;
//! * a client-token record is released with the resource it minted, so once a rule is
//!   gone its token is free again, while a record whose rule is still stored keeps
//!   replaying that rule;
//! * every refusal for a gateway, a link or a rule this store does not hold is the
//!   service's one not-found: HTTP 404 with fixed text naming nothing from the
//!   request.
//!
//! The tables span what the rules generalise over: each condition member the modelled
//! `RuleCondition` declares (and a structure stating none at all), both published
//! bounds of `RulePriority` (1..1000) and a value past each of them, and which of the
//! link's two ends addresses the call
//! (`smithy_query(query="shape", shape="RuleCondition,RulePriority")`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    RESOURCE_NOT_FOUND_MESSAGE, RULE_STATUS_ACTIVE, RULE_STATUS_CREATION_IN_PROGRESS,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for, and so the region every ARN it builds names.
const REGION: &str = "us-west-2";

/// How long a wait for a clock-owed transition is given before the test gives up.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between two reads of the same record.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

/// A `CreateRequesterGateway` body whose members satisfy the modelled patterns, so the
/// same token can be presented twice to see what its record answers.
fn requester_gateway_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "clientToken": client_token,
    })
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

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles.
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
                Some(requester_gateway_body(&format!("{token}-requester"))),
            )
            .await;
        assert_eq!(
            requester.status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
            requester.status, requester.body
        );
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
                    "clientToken": format!("{token}-responder"),
                })),
            )
            .await;
        assert_eq!(
            responder.status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {} {:?}",
            responder.status, responder.body
        );
        let requester_id = text(&requester.body, "gatewayId");
        let responder_id = text(&responder.body, "gatewayId");
        let link_id = self.create_link(case, &requester_id, &responder_id).await;
        Pair {
            requester_id,
            responder_id,
            link_id,
        }
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

    async fn create_rule(
        &self,
        gateway_id: &str,
        link_id: &str,
        token: &str,
        priority: i64,
        conditions: Value,
        tags: Option<Value>,
    ) -> Answer {
        let mut body = json!({
            "clientToken": token,
            "priority": priority,
            "conditions": conditions,
        });
        if let Some(tags) = tags {
            body["tags"] = tags;
        }
        self.send(
            "POST",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule"),
            Some(body),
        )
        .await
    }

    async fn get_rule(&self, gateway_id: &str, link_id: &str, rule_id: &str) -> Answer {
        self.send(
            "GET",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            None,
        )
        .await
    }

    /// Read the rule until it reports `expected`, or a generous deadline passes: the
    /// transition is one the clock owes, so the test waits for the CONDITION rather
    /// than for a span of time.
    async fn settled(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
        rule_id: &str,
        expected: &str,
    ) -> Value {
        let give_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            let answered = self.get_rule(gateway_id, link_id, rule_id).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected GetLinkRoutingRule to answer 200, got {} {:?}",
                answered.status, answered.body
            );
            if text(&answered.body, "status") == expected {
                return answered.body;
            }
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected the rule to settle to {expected:?}, got {:?}",
                answered.body["status"]
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }
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

/// One row: a condition the rule matches on, as the modelled `RuleCondition`
/// declares its members, and the priority the rule holds.
struct Row {
    name: &'static str,
    conditions: fn() -> Value,
    priority: i64,
}

/// Every member of `RuleCondition`, one per row, plus one row stating two of them
/// together (the shape's documented AND), at priorities spanning both published
/// bounds of `RulePriority`.
const ROWS: &[Row] = &[
    Row {
        name: "an exact host header at the lowest priority",
        conditions: || json!({ "hostHeader": "bids.example.com" }),
        priority: 1,
    },
    Row {
        name: "a wildcard host header",
        conditions: || json!({ "hostHeaderWildcard": "*.example.com" }),
        priority: 2,
    },
    Row {
        name: "a path prefix",
        conditions: || json!({ "pathPrefix": "/bid" }),
        priority: 500,
    },
    Row {
        name: "an exact path",
        conditions: || json!({ "pathExact": "/bid/openrtb" }),
        priority: 999,
    },
    Row {
        name: "a query string pair",
        conditions: || json!({ "queryStringEquals": { "key": "format", "value": "openrtb" } }),
        priority: 1000,
    },
    Row {
        name: "a query string key that must be present",
        conditions: || json!({ "queryStringExists": "debug" }),
        priority: 42,
    },
    Row {
        name: "a host header and a path prefix together",
        conditions: || json!({ "hostHeader": "bids.example.com", "pathPrefix": "/bid" }),
        priority: 7,
    },
];

#[tokio::test]
async fn a_rule_round_trips_through_its_link_from_either_end() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case, "token").await;
        let conditions = (row.conditions)();

        let created = fabric
            .create_rule(
                &pair.responder_id,
                &pair.link_id,
                "token-rule",
                row.priority,
                conditions.clone(),
                None,
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateLinkRoutingRule to answer 200, got {} {:?}",
            created.status, created.body
        );
        let rule_id = text(&created.body, "ruleId");
        assert!(
            rule_id.starts_with("rule-"),
            "case {case:?}: expected a minted ruleId, got {rule_id:?}"
        );
        assert_eq!(
            text(&created.body, "status"),
            RULE_STATUS_CREATION_IN_PROGRESS,
            "case {case:?}: expected a fresh rule to be creating, got {:?}",
            created.body["status"]
        );

        // Either end of the link addresses the same record, and what comes back is
        // what was stored.
        for (end, gateway_id) in [
            ("the responder end", &pair.responder_id),
            ("the requester end", &pair.requester_id),
        ] {
            let read = fabric.get_rule(gateway_id, &pair.link_id, &rule_id).await;
            assert_eq!(
                read.status, 200,
                "case {case:?}: expected GetLinkRoutingRule from {end} to answer 200, got {} {:?}",
                read.status, read.body
            );
            assert_eq!(
                read.body["conditions"], conditions,
                "case {case:?}: expected {end} to read the stored conditions {conditions:?}, got {:?}",
                read.body["conditions"]
            );
            assert_eq!(
                read.body["priority"],
                json!(row.priority),
                "case {case:?}: expected {end} to read priority {}, got {:?}",
                row.priority,
                read.body["priority"]
            );
            assert_eq!(
                text(&read.body, "linkId"),
                pair.link_id,
                "case {case:?}: expected {end} to read link {:?}, got {:?}",
                pair.link_id,
                read.body["linkId"]
            );
            assert_eq!(
                text(&read.body, "gatewayId").as_str(),
                gateway_id.as_str(),
                "case {case:?}: expected {end} to be reported as the addressed gateway, got {:?}",
                read.body["gatewayId"]
            );
            assert_eq!(
                read.body["createdAt"], created.body["createdAt"],
                "case {case:?}: expected {end} to read the create's createdAt {:?}, got {:?}",
                created.body["createdAt"], read.body["createdAt"]
            );
        }

        // The clock settles the rule, and the settled record keeps its own facts.
        let settled = fabric
            .settled(
                case,
                &pair.responder_id,
                &pair.link_id,
                &rule_id,
                RULE_STATUS_ACTIVE,
            )
            .await;
        assert_eq!(
            settled["conditions"], conditions,
            "case {case:?}: expected the settled rule to keep its conditions, got {:?}",
            settled["conditions"]
        );
        assert_eq!(
            settled["createdAt"], created.body["createdAt"],
            "case {case:?}: expected the settled rule to keep its createdAt, got {:?}",
            settled["createdAt"]
        );
    }
}

#[tokio::test]
async fn a_repeat_of_one_client_token_answers_the_rule_it_minted() {
    let fabric = Fabric::new();
    let case = "one client token, two calls";
    let pair = fabric.linked_pair(case, "token").await;
    let conditions = json!({ "pathPrefix": "/bid" });

    let first = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            10,
            conditions.clone(),
            None,
        )
        .await;
    let repeat = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            10,
            conditions.clone(),
            None,
        )
        .await;
    assert_eq!(
        repeat.body["ruleId"], first.body["ruleId"],
        "case {case:?}: expected the repeat to answer the first rule {:?}, got {:?}",
        first.body["ruleId"], repeat.body["ruleId"]
    );
    assert_eq!(
        repeat.body["createdAt"], first.body["createdAt"],
        "case {case:?}: expected the repeat to answer the first rule's createdAt, got {:?}",
        repeat.body["createdAt"]
    );

    // The same token with different parameters is refused rather than answering the
    // first call's rule.
    let mismatched = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            11,
            conditions,
            None,
        )
        .await;
    assert_eq!(
        mismatched.status, 400,
        "case {case:?}: expected a token replayed with other parameters to be refused, got {} {:?}",
        mismatched.status, mismatched.body
    );
    assert_eq!(
        mismatched.code, "ValidationException",
        "case {case:?}: expected ValidationException, got {:?}",
        mismatched.code
    );
}

#[tokio::test]
async fn a_rule_the_store_does_not_hold_is_not_found() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("the stored pair", "token-a").await;
    // A second pair, so there is a link that carries no rule of the first link's id
    // and a gateway that is no end of the first link.
    let other = fabric.linked_pair("the other pair", "token-b").await;
    let created = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            1,
            json!({ "pathPrefix": "/bid" }),
            None,
        )
        .await;
    let rule_id = text(&created.body, "ruleId");

    for (case, gateway_id, link_id, addressed_rule) in [
        (
            "nothing of the three exists",
            "rtb-gw-0123456789abcdef",
            "link-0123456789abcdef",
            "rule-0123456789abcdef",
        ),
        (
            "a stored link and a rule id nothing minted",
            pair.responder_id.as_str(),
            pair.link_id.as_str(),
            "rule-0123456789abcdef",
        ),
        (
            "a stored rule addressed through another link",
            other.responder_id.as_str(),
            other.link_id.as_str(),
            rule_id.as_str(),
        ),
        (
            "a stored rule addressed from a gateway that is no end of its link",
            other.requester_id.as_str(),
            pair.link_id.as_str(),
            rule_id.as_str(),
        ),
    ] {
        let read = fabric.get_rule(gateway_id, link_id, addressed_rule).await;
        assert_not_found(case, &read, &[gateway_id, link_id, addressed_rule]);
    }

    // Creating a rule on a link the store does not hold is the same not-found.
    for (case, gateway_id, link_id) in [
        ("creating on a link id nothing minted", pair.responder_id.as_str(), "link-0123456789abcdef"),
        (
            "creating on a link the gateway is no end of",
            other.requester_id.as_str(),
            pair.link_id.as_str(),
        ),
    ] {
        let answered = fabric
            .create_rule(
                gateway_id,
                link_id,
                "token-rule-unknown",
                3,
                json!({ "pathPrefix": "/bid" }),
                None,
            )
            .await;
        assert_not_found(case, &answered, &[gateway_id, link_id]);
    }
}

#[tokio::test]
async fn a_rule_must_state_a_condition_and_a_priority_inside_its_bounds() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("the stored pair", "token").await;

    // `conditions` carries only optional members, so a structure stating none at all
    // reaches the handler and is refused there.
    let empty = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule-empty",
            1,
            json!({}),
            None,
        )
        .await;
    assert_eq!(
        empty.status, 400,
        "case {:?}: expected a rule matching on nothing to be refused, got {} {:?}",
        "no condition at all", empty.status, empty.body
    );
    assert_eq!(
        empty.code, "ValidationException",
        "case {:?}: expected ValidationException, got {:?}",
        "no condition at all", empty.code
    );

    // The published bounds of `RulePriority` (1..1000), and one value past each.
    for (case, priority, acceptable) in [
        ("the lowest priority", 1, true),
        ("the highest priority", 1000, true),
        ("one below the lowest", 0, false),
        ("one above the highest", 1001, false),
    ] {
        let answered = fabric
            .create_rule(
                &pair.responder_id,
                &pair.link_id,
                &format!("token-rule-{priority}"),
                priority,
                json!({ "pathPrefix": "/bid" }),
                None,
            )
            .await;
        if acceptable {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected priority {priority} to be accepted, got {} {:?}",
                answered.status, answered.body
            );
        } else {
            assert_eq!(
                answered.status, 400,
                "case {case:?}: expected priority {priority} to be refused, got {} {:?}",
                answered.status, answered.body
            );
        }
    }
}

#[tokio::test]
async fn a_rules_tags_are_readable_through_the_tagging_surface() {
    let fabric = Fabric::new();
    let case = "a rule carrying tags";
    let pair = fabric.linked_pair(case, "token").await;
    let created = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            1,
            json!({ "pathPrefix": "/bid" }),
            Some(json!({ "tier": "gold" })),
        )
        .await;
    let rule_id = text(&created.body, "ruleId");

    let read = fabric
        .get_rule(&pair.responder_id, &pair.link_id, &rule_id)
        .await;
    assert_eq!(
        read.body["tags"],
        json!({ "tier": "gold" }),
        "case {case:?}: expected the rule to report its stored tags, got {:?}",
        read.body["tags"]
    );

    // The same record through `/tags/{resourceArn}`: the routing-rule ARN the
    // modelled `RtbTaggableResourceArn` pattern spells out, in the account and
    // Region this emulator serves.
    let arn = ::engine::emulator_arn(
        "rtbfabric",
        REGION,
        &format!(
            "gateway/{}/link/{}/routing-rule/{rule_id}",
            pair.responder_id, pair.link_id
        ),
    );
    let encoded = utf8_percent_encode(&arn, NON_ALPHANUMERIC).to_string();
    let listed = fabric.send("GET", &format!("/tags/{encoded}"), None).await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected ListTagsForResource to answer 200, got {} {:?}",
        listed.status, listed.body
    );
    assert_eq!(
        listed.body["tags"],
        json!({ "tier": "gold" }),
        "case {case:?}: expected the tagging surface to report the rule's tags, got {:?}",
        listed.body["tags"]
    );

    // A tag written through that surface is on the same record the rule read reports.
    let tagged = fabric
        .send(
            "POST",
            &format!("/tags/{encoded}"),
            Some(json!({ "tags": { "owner": "ads" } })),
        )
        .await;
    assert_eq!(
        tagged.status, 204,
        "case {case:?}: expected TagResource to answer 204, got {} {:?}",
        tagged.status, tagged.body
    );
    let read_again = fabric
        .get_rule(&pair.responder_id, &pair.link_id, &rule_id)
        .await;
    assert_eq!(
        read_again.body["tags"],
        json!({ "owner": "ads", "tier": "gold" }),
        "case {case:?}: expected the rule to report both tags, got {:?}",
        read_again.body["tags"]
    );
}

#[tokio::test]
async fn a_rule_does_not_outlive_the_link_it_routes_for() {
    let fabric = Fabric::new();
    let case = "the link carrying the rule is deleted";
    let pair = fabric.linked_pair(case, "token").await;
    let created = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            1,
            json!({ "pathPrefix": "/bid" }),
            None,
        )
        .await;
    let rule_id = text(&created.body, "ruleId");

    let deleted = fabric
        .send(
            "DELETE",
            &format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected DeleteLink to answer 200, got {} {:?}",
        deleted.status, deleted.body
    );

    let read = fabric
        .get_rule(&pair.responder_id, &pair.link_id, &rule_id)
        .await;
    assert_not_found(case, &read, &[&pair.responder_id, &pair.link_id, &rule_id]);
}

/// How the resources a routing rule's client-token record belongs to are released.
enum Release {
    /// `DeleteLink`, addressed from the end named here.
    Link { from_requester: bool },
    /// `DeleteRequesterGateway`, which takes the link and so the rule with it.
    RequesterGateway,
    /// `DeleteResponderGateway`, the other end of the same link.
    ResponderGateway,
}

/// One row of the release table: the call that releases the rule.
struct ReleaseRow {
    name: &'static str,
    release: Release,
}

/// Every call this service serves that releases a routing rule: the rule's own link
/// from either of its two ends, and either gateway the link joins.
const RELEASE_ROWS: &[ReleaseRow] = &[
    ReleaseRow {
        name: "the link deleted from its requester end",
        release: Release::Link {
            from_requester: true,
        },
    },
    ReleaseRow {
        name: "the link deleted from its responder end",
        release: Release::Link {
            from_requester: false,
        },
    },
    ReleaseRow {
        name: "the requester gateway deleted",
        release: Release::RequesterGateway,
    },
    ReleaseRow {
        name: "the responder gateway deleted",
        release: Release::ResponderGateway,
    },
];

#[tokio::test]
async fn a_rules_client_token_is_released_with_the_rule() {
    // One condition and priority throughout: what varies between the rows is which
    // call releases the rule, not what the rule holds.
    let conditions = json!({ "pathPrefix": "/bid" });
    let priority = 10;

    for row in RELEASE_ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case, "token-first").await;
        let created = fabric
            .create_rule(
                &pair.responder_id,
                &pair.link_id,
                "token-rule",
                priority,
                conditions.clone(),
                None,
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateLinkRoutingRule to answer 200, got {} {:?}",
            created.status, created.body
        );
        let released_rule_id = text(&created.body, "ruleId");

        let path = match row.release {
            Release::Link { from_requester: true } => {
                format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id)
            }
            Release::Link {
                from_requester: false,
            } => format!("/gateway/{}/link/{}", pair.responder_id, pair.link_id),
            Release::RequesterGateway => format!("/requester-gateway/{}", pair.requester_id),
            Release::ResponderGateway => format!("/responder-gateway/{}", pair.responder_id),
        };
        let deleted = fabric.send("DELETE", &path, None).await;
        assert_eq!(
            deleted.status, 200,
            "case {case:?}: expected the delete to answer 200, got {} {:?}",
            deleted.status, deleted.body
        );

        // The rule the token minted is gone, so nothing the record points at is
        // readable any more.
        let read = fabric
            .get_rule(&pair.responder_id, &pair.link_id, &released_rule_id)
            .await;
        assert_not_found(
            case,
            &read,
            &[&pair.responder_id, &pair.link_id, &released_rule_id],
        );

        // A fresh link to carry the next rule: the same gateway pair when only the
        // link went, a new pair when a gateway went with it.
        let (gateway_id, link_id) = match row.release {
            Release::Link { .. } => {
                let link_id = fabric
                    .create_link(case, &pair.requester_id, &pair.responder_id)
                    .await;
                (pair.responder_id.clone(), link_id)
            }
            Release::RequesterGateway | Release::ResponderGateway => {
                let next = fabric.linked_pair(case, "token-second").await;
                (next.responder_id.clone(), next.link_id.clone())
            }
        };

        // The record went with the rule, so the token is free: the create mints a
        // second rule rather than being refused for parameters that no longer
        // describe anything the store holds.
        let reused = fabric
            .create_rule(
                &gateway_id,
                &link_id,
                "token-rule",
                priority,
                conditions.clone(),
                None,
            )
            .await;
        assert_eq!(
            reused.status, 200,
            "case {case:?}: expected the freed token to mint a rule, got {} {:?}",
            reused.status, reused.body
        );
        let minted_rule_id = text(&reused.body, "ruleId");
        assert_ne!(
            minted_rule_id, released_rule_id,
            "case {case:?}: expected a newly minted ruleId, got the released {released_rule_id:?}"
        );

        // And what it minted is really stored: readable through its own link.
        let read_back = fabric.get_rule(&gateway_id, &link_id, &minted_rule_id).await;
        assert_eq!(
            read_back.status, 200,
            "case {case:?}: expected the new rule to read back, got {} {:?}",
            read_back.status, read_back.body
        );
        assert_eq!(
            text(&read_back.body, "linkId"),
            link_id,
            "case {case:?}: expected the new rule to route for link {link_id:?}, got {:?}",
            read_back.body["linkId"]
        );
        assert_eq!(
            read_back.body["conditions"], conditions,
            "case {case:?}: expected the new rule to hold the created conditions, got {:?}",
            read_back.body["conditions"]
        );
    }
}

#[tokio::test]
async fn a_client_token_whose_resource_is_still_stored_keeps_replaying_it() {
    let fabric = Fabric::new();
    let case = "a release elsewhere in the store";
    let conditions = json!({ "hostHeader": "bids.example.com" });
    let kept = fabric.linked_pair(case, "token-kept").await;
    // A second pair, whose link is what the test releases.
    let released = fabric.linked_pair(case, "token-released").await;

    let created = fabric
        .create_rule(
            &kept.responder_id,
            &kept.link_id,
            "token-rule",
            7,
            conditions.clone(),
            None,
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected CreateLinkRoutingRule to answer 200, got {} {:?}",
        created.status, created.body
    );

    let deleted = fabric
        .send(
            "DELETE",
            &format!(
                "/gateway/{}/link/{}",
                released.requester_id, released.link_id
            ),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected DeleteLink to answer 200, got {} {:?}",
        deleted.status, deleted.body
    );

    // Only the released resources' records go: the rule this token minted is still
    // stored, so the replay answers it rather than minting a second rule.
    let replay = fabric
        .create_rule(
            &kept.responder_id,
            &kept.link_id,
            "token-rule",
            7,
            conditions,
            None,
        )
        .await;
    assert_eq!(
        replay.status, 200,
        "case {case:?}: expected the replay to answer 200, got {} {:?}",
        replay.status, replay.body
    );
    assert_eq!(
        replay.body["ruleId"], created.body["ruleId"],
        "case {case:?}: expected the replay to answer the stored rule {:?}, got {:?}",
        created.body["ruleId"], replay.body["ruleId"]
    );
    assert_eq!(
        replay.body["createdAt"], created.body["createdAt"],
        "case {case:?}: expected the replay to answer the stored rule's createdAt {:?}, got {:?}",
        created.body["createdAt"], replay.body["createdAt"]
    );

    // The same holds for a gateway that is still stored: the delete above released a
    // link, not either gateway of the kept pair.
    let gateway_replay = fabric
        .send(
            "POST",
            "/requester-gateway",
            Some(requester_gateway_body("token-kept-requester")),
        )
        .await;
    assert_eq!(
        gateway_replay.status, 200,
        "case {case:?}: expected CreateRequesterGateway to answer 200, got {} {:?}",
        gateway_replay.status, gateway_replay.body
    );
    assert_eq!(
        text(&gateway_replay.body, "gatewayId"),
        kept.requester_id,
        "case {case:?}: expected the replay to answer the stored gateway {:?}, got {:?}",
        kept.requester_id,
        gateway_replay.body["gatewayId"]
    );
}
