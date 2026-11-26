// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `UpdateLinkRoutingRule`, driven over the wire through the
//! router the production emulator uses and read back through `GetLinkRoutingRule`.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * `priority` and `conditions` are both `@required`, so an update states the whole
//!   configuration the rule carries from then on: the read reports exactly what the
//!   update stored, the rule keeps the id and the `createdAt` the create minted, and
//!   the stored `updatedAt` moves to the instant of the update;
//! * a rule that was routing takes the change live, so the call answers
//!   `UPDATE_IN_PROGRESS` and the clock then owes the rule `ACTIVE` again, while a
//!   rule that has not finished coming up keeps `CREATION_IN_PROGRESS`;
//! * one link holds one rule per priority among its non-deleted rules, so an update
//!   asking for a number ANOTHER of that link's rules holds is refused with the
//!   modelled `ConflictException` (HTTP 409) and stores nothing, while the number the
//!   updated rule already holds, and the same number on another link, are free;
//! * a rule that matches on nothing matches everything, so an update stating no
//!   condition at all is refused with the modelled `ValidationException` (HTTP 400)
//!   and stores nothing;
//! * a rule whose deletion is outstanding has no configuration left to change, so the
//!   update is refused with `ConflictException` naming the status that refuses it;
//! * every refusal for a gateway, a link or a rule this store does not hold is the
//!   service's one not-found: HTTP 404 with fixed text naming nothing from the
//!   request.
//!
//! The tables span what the rules generalise over: the condition members the modelled
//! `RuleCondition` declares, both published bounds of `RulePriority` (1..1000) and a
//! value past each of them, and which of the link's two ends addresses the call
//! (`smithy_query(query="shape", shape="RuleCondition,RulePriority")`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    priority_conflict_message, RESOURCE_NOT_FOUND_MESSAGE, RULE_CONDITIONS_EMPTY_MESSAGE,
    RULE_STATUS_ACTIVE, RULE_STATUS_CREATION_IN_PROGRESS, RULE_STATUS_DELETION_IN_PROGRESS,
    RULE_STATUS_UPDATE_IN_PROGRESS,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
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

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// A timestamp member as the wire carries it, for comparing two readings of the same
/// record rather than for asserting any particular instant.
fn instant(body: &Value, member: &str) -> f64 {
    body[member]
        .as_f64()
        .unwrap_or_else(|| panic!("expected {member} to be a timestamp, got {:?}", body[member]))
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
        Pair {
            requester_id,
            responder_id,
            link_id: text(&link.body, "linkId"),
        }
    }

    async fn create_rule(
        &self,
        gateway_id: &str,
        link_id: &str,
        token: &str,
        priority: i64,
        conditions: Value,
    ) -> Answer {
        self.send(
            "POST",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule"),
            Some(json!({
                "clientToken": token,
                "priority": priority,
                "conditions": conditions,
            })),
        )
        .await
    }

    /// Create a rule and wait until the clock has put it in service, so a call made
    /// after this one acts on a rule that is routing.
    async fn routing_rule(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
        token: &str,
        priority: i64,
        conditions: Value,
    ) -> String {
        let created = self
            .create_rule(gateway_id, link_id, token, priority, conditions)
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateLinkRoutingRule to answer 200, got {} {:?}",
            created.status, created.body
        );
        let rule_id = text(&created.body, "ruleId");
        self.settled(case, gateway_id, link_id, &rule_id, RULE_STATUS_ACTIVE)
            .await;
        rule_id
    }

    async fn get_rule(&self, gateway_id: &str, link_id: &str, rule_id: &str) -> Answer {
        self.send(
            "GET",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            None,
        )
        .await
    }

    async fn update_rule(
        &self,
        gateway_id: &str,
        link_id: &str,
        rule_id: &str,
        priority: i64,
        conditions: Value,
    ) -> Answer {
        self.send(
            "PUT",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            Some(json!({ "priority": priority, "conditions": conditions })),
        )
        .await
    }

    async fn delete_rule(&self, gateway_id: &str, link_id: &str, rule_id: &str) -> Answer {
        self.send(
            "DELETE",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            None,
        )
        .await
    }

    /// The rule as `GetLinkRoutingRule` reports it, which has to be an answer.
    async fn read_rule(&self, case: &str, gateway_id: &str, link_id: &str, rule_id: &str) -> Value {
        let answered = self.get_rule(gateway_id, link_id, rule_id).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected GetLinkRoutingRule to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.body
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
            let body = self.read_rule(case, gateway_id, link_id, rule_id).await;
            if text(&body, "status") == expected {
                return body;
            }
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected the rule to settle to {expected:?}, got {:?}",
                body["status"]
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// Read the rule until it is gone, or a generous deadline passes.
    async fn gone(&self, case: &str, gateway_id: &str, link_id: &str, rule_id: &str) {
        let give_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            let answered = self.get_rule(gateway_id, link_id, rule_id).await;
            if answered.status != 200 {
                assert_not_found(case, &answered, &[gateway_id, link_id, rule_id]);
                return;
            }
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected the rule to be gone, got {:?}",
                answered.body
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

/// One row: the configuration a rule is created with, the configuration an update
/// moves it to, and which of the link's two ends addresses the update.
struct Row {
    name: &'static str,
    created_priority: i64,
    created_conditions: fn() -> Value,
    updated_priority: i64,
    updated_conditions: fn() -> Value,
    /// Whether the update is addressed through the responder end of the link; the
    /// other end is the requester gateway, and a rule is addressable from either.
    from_responder: bool,
}

/// Rows differing along what the rule generalises over: which `RuleCondition`
/// members the update states (one, several, a different one than the create stored,
/// the same one with another value), both published bounds of `RulePriority` and
/// values between them, and the end of the link the call is addressed to.
const ROWS: &[Row] = &[
    Row {
        name: "an exact host header replaced by a wildcard, at the lowest priority",
        created_priority: 500,
        created_conditions: || json!({ "hostHeader": "bids.example.com" }),
        updated_priority: 1,
        updated_conditions: || json!({ "hostHeaderWildcard": "*.example.com" }),
        from_responder: true,
    },
    Row {
        name: "a path prefix narrowed to an exact path, at the highest priority",
        created_priority: 2,
        created_conditions: || json!({ "pathPrefix": "/bid" }),
        updated_priority: 1000,
        updated_conditions: || json!({ "pathExact": "/bid/openrtb" }),
        from_responder: false,
    },
    Row {
        name: "one condition widened to three",
        created_priority: 7,
        created_conditions: || json!({ "queryStringExists": "debug" }),
        updated_priority: 8,
        updated_conditions: || {
            json!({
                "hostHeader": "bids.example.com",
                "pathPrefix": "/bid",
                "queryStringEquals": { "key": "format", "value": "openrtb" },
            })
        },
        from_responder: true,
    },
    Row {
        name: "a query string pair re-pointed, keeping the priority it holds",
        created_priority: 42,
        created_conditions: || json!({ "queryStringEquals": { "key": "format", "value": "vast" } }),
        updated_priority: 42,
        updated_conditions: || json!({ "queryStringEquals": { "key": "format", "value": "openrtb" } }),
        from_responder: false,
    },
    Row {
        name: "several conditions narrowed back to one",
        created_priority: 999,
        created_conditions: || {
            json!({ "hostHeaderWildcard": "*.example.com", "pathExact": "/bid/openrtb" })
        },
        updated_priority: 3,
        updated_conditions: || json!({ "pathPrefix": "/bid" }),
        from_responder: true,
    },
];

#[tokio::test]
async fn an_update_replaces_the_rules_whole_configuration() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case, "token").await;
        let end = if row.from_responder {
            &pair.responder_id
        } else {
            &pair.requester_id
        };

        let rule_id = fabric
            .routing_rule(
                case,
                &pair.responder_id,
                &pair.link_id,
                "token-rule",
                row.created_priority,
                (row.created_conditions)(),
            )
            .await;
        let created = fabric
            .read_rule(case, &pair.responder_id, &pair.link_id, &rule_id)
            .await;

        let updated = fabric
            .update_rule(
                end,
                &pair.link_id,
                &rule_id,
                row.updated_priority,
                (row.updated_conditions)(),
            )
            .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected UpdateLinkRoutingRule to answer 200, got {} {:?}",
            updated.status, updated.body
        );
        assert_eq!(
            text(&updated.body, "ruleId"),
            rule_id,
            "case {case:?}: expected the update to answer rule {rule_id:?}, got {:?}",
            updated.body["ruleId"]
        );
        // The rule was routing, so the change is applied to a live rule: the call
        // reports it as pending and the clock owes the rule `ACTIVE` again.
        assert_eq!(
            text(&updated.body, "status"),
            RULE_STATUS_UPDATE_IN_PROGRESS,
            "case {case:?}: expected the update to answer {RULE_STATUS_UPDATE_IN_PROGRESS:?}, got {:?}",
            updated.body["status"]
        );
        assert!(
            instant(&updated.body, "updatedAt") >= instant(&created, "updatedAt"),
            "case {case:?}: expected updatedAt to move to the update, got {:?} after {:?}",
            updated.body["updatedAt"],
            created["updatedAt"]
        );

        // The read reports exactly what the update stated, and nothing the update
        // did not speak of changed.
        let read = fabric
            .read_rule(case, &pair.requester_id, &pair.link_id, &rule_id)
            .await;
        assert_eq!(
            read["priority"], row.updated_priority,
            "case {case:?}: expected priority {}, got {:?}",
            row.updated_priority, read["priority"]
        );
        assert_eq!(
            read["conditions"],
            (row.updated_conditions)(),
            "case {case:?}: expected the stated conditions {:?}, got {:?}",
            (row.updated_conditions)(),
            read["conditions"]
        );
        assert_eq!(
            read["createdAt"], created["createdAt"],
            "case {case:?}: expected the create's createdAt {:?} to stand, got {:?}",
            created["createdAt"], read["createdAt"]
        );
        assert_eq!(
            read["updatedAt"], updated.body["updatedAt"],
            "case {case:?}: expected the read to report the update's updatedAt {:?}, got {:?}",
            updated.body["updatedAt"], read["updatedAt"]
        );
        assert_eq!(
            text(&read, "ruleId"),
            rule_id,
            "case {case:?}: expected the rule to keep its id {rule_id:?}, got {:?}",
            read["ruleId"]
        );

        // The clock then puts the change in service, and the configuration the
        // update stored is still what the rule carries.
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
            settled["priority"], row.updated_priority,
            "case {case:?}: expected the settled rule to keep priority {}, got {:?}",
            row.updated_priority, settled["priority"]
        );
        assert_eq!(
            settled["conditions"],
            (row.updated_conditions)(),
            "case {case:?}: expected the settled rule to keep the stated conditions, got {:?}",
            settled["conditions"]
        );
    }
}

#[tokio::test]
async fn a_rule_that_has_not_come_up_yet_keeps_its_creating_status() {
    let case = "an update while the rule is still coming up";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;

    let created = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            4,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected CreateLinkRoutingRule to answer 200, got {} {:?}",
        created.status, created.body
    );
    assert_eq!(
        text(&created.body, "status"),
        RULE_STATUS_CREATION_IN_PROGRESS,
        "case {case:?}: expected the create to answer {RULE_STATUS_CREATION_IN_PROGRESS:?}, got {:?}",
        created.body["status"]
    );
    let rule_id = text(&created.body, "ruleId");

    // The rule has not routed yet, so the change is in what it comes up with: the
    // status it already stands in is the one reported, and the transition the clock
    // owes it still ends at `ACTIVE`.
    let updated = fabric
        .update_rule(
            &pair.responder_id,
            &pair.link_id,
            &rule_id,
            5,
            json!({ "pathExact": "/bid/openrtb" }),
        )
        .await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected UpdateLinkRoutingRule to answer 200, got {} {:?}",
        updated.status, updated.body
    );
    assert!(
        [RULE_STATUS_CREATION_IN_PROGRESS, RULE_STATUS_ACTIVE]
            .contains(&text(&updated.body, "status").as_str()),
        "case {case:?}: expected the rule to keep the status it was coming up in, got {:?}",
        updated.body["status"]
    );

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
        settled["priority"], 5,
        "case {case:?}: expected the settled rule to carry priority 5, got {:?}",
        settled["priority"]
    );
    assert_eq!(
        settled["conditions"],
        json!({ "pathExact": "/bid/openrtb" }),
        "case {case:?}: expected the settled rule to carry the stated conditions, got {:?}",
        settled["conditions"]
    );
}

#[tokio::test]
async fn one_link_holds_one_rule_per_priority() {
    let case = "a priority another rule of the link holds";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;
    let other_pair = fabric.linked_pair(case, "other-token").await;

    let moved = fabric
        .routing_rule(
            case,
            &pair.responder_id,
            &pair.link_id,
            "token-rule-moved",
            10,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;
    let holder = fabric
        .routing_rule(
            case,
            &pair.responder_id,
            &pair.link_id,
            "token-rule-holder",
            20,
            json!({ "pathPrefix": "/stat" }),
        )
        .await;
    // Another link's rules are no constraint on this link's numbers.
    let elsewhere = fabric
        .routing_rule(
            case,
            &other_pair.responder_id,
            &other_pair.link_id,
            "other-token-rule",
            30,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;

    let refused = fabric
        .update_rule(
            &pair.responder_id,
            &pair.link_id,
            &moved,
            20,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;
    assert_eq!(
        refused.status, 409,
        "case {case:?}: expected HTTP 409, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.code, "ConflictException",
        "case {case:?}: expected ConflictException, got {:?}",
        refused.code
    );
    assert_eq!(
        refused.message(),
        priority_conflict_message(&holder, &pair.link_id, 20),
        "case {case:?}: expected the refusal to name the rule holding the number, got {:?}",
        refused.message()
    );

    // The refusal stored nothing: both rules carry the numbers they held.
    let unchanged = fabric
        .read_rule(case, &pair.responder_id, &pair.link_id, &moved)
        .await;
    assert_eq!(
        unchanged["priority"], 10,
        "case {case:?}: expected the refused rule to keep priority 10, got {:?}",
        unchanged["priority"]
    );
    let held = fabric
        .read_rule(case, &pair.responder_id, &pair.link_id, &holder)
        .await;
    assert_eq!(
        held["priority"], 20,
        "case {case:?}: expected the holding rule to keep priority 20, got {:?}",
        held["priority"]
    );

    // The number a rule already holds is its own, so re-stating it is no conflict.
    let restated = fabric
        .update_rule(
            &pair.responder_id,
            &pair.link_id,
            &moved,
            10,
            json!({ "pathExact": "/bid/openrtb" }),
        )
        .await;
    assert_eq!(
        restated.status, 200,
        "case {case:?}: expected a rule to keep its own number, got {} {:?}",
        restated.status, restated.body
    );

    // And the same number on another link is free.
    let across = fabric
        .update_rule(
            &pair.responder_id,
            &pair.link_id,
            &moved,
            30,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;
    assert_eq!(
        across.status, 200,
        "case {case:?}: expected another link's number to be free, got {} {:?}",
        across.status, across.body
    );
    let elsewhere_read = fabric
        .read_rule(
            case,
            &other_pair.responder_id,
            &other_pair.link_id,
            &elsewhere,
        )
        .await;
    assert_eq!(
        elsewhere_read["priority"], 30,
        "case {case:?}: expected the other link's rule to be untouched, got {:?}",
        elsewhere_read["priority"]
    );
}

#[tokio::test]
async fn an_update_stating_no_condition_is_refused() {
    let case = "no condition at all";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;
    let rule_id = fabric
        .routing_rule(
            case,
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            11,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;

    let refused = fabric
        .update_rule(&pair.responder_id, &pair.link_id, &rule_id, 12, json!({}))
        .await;
    assert_eq!(
        refused.status, 400,
        "case {case:?}: expected HTTP 400, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.code, "ValidationException",
        "case {case:?}: expected ValidationException, got {:?}",
        refused.code
    );
    assert_eq!(
        refused.message(),
        RULE_CONDITIONS_EMPTY_MESSAGE,
        "case {case:?}: expected {RULE_CONDITIONS_EMPTY_MESSAGE:?}, got {:?}",
        refused.message()
    );

    let read = fabric
        .read_rule(case, &pair.responder_id, &pair.link_id, &rule_id)
        .await;
    assert_eq!(
        read["priority"], 11,
        "case {case:?}: expected the refusal to store nothing, got {:?}",
        read["priority"]
    );
    assert_eq!(
        read["conditions"],
        json!({ "pathPrefix": "/bid" }),
        "case {case:?}: expected the refusal to store nothing, got {:?}",
        read["conditions"]
    );
}

#[tokio::test]
async fn the_published_priority_bounds_frame_an_update() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("the stored pair", "token").await;
    let rule_id = fabric
        .routing_rule(
            "the stored pair",
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            500,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;

    // The published bounds of `RulePriority` (1..1000), and one value past each.
    for (case, priority, acceptable) in [
        ("the lowest priority", 1, true),
        ("the highest priority", 1000, true),
        ("one below the lowest", 0, false),
        ("one above the highest", 1001, false),
    ] {
        let answered = fabric
            .update_rule(
                &pair.responder_id,
                &pair.link_id,
                &rule_id,
                priority,
                json!({ "pathPrefix": "/bid" }),
            )
            .await;
        if acceptable {
            assert_eq!(
                answered.status, 200,
                "case {case:?}: expected priority {priority} to be accepted, got {} {:?}",
                answered.status, answered.body
            );
            let read = fabric
                .read_rule(case, &pair.responder_id, &pair.link_id, &rule_id)
                .await;
            assert_eq!(
                read["priority"], priority,
                "case {case:?}: expected the rule to carry priority {priority}, got {:?}",
                read["priority"]
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
async fn a_rule_this_store_does_not_hold_is_not_found() {
    let case = "a rule this store does not hold";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;
    let other_pair = fabric.linked_pair(case, "other-token").await;
    let rule_id = fabric
        .routing_rule(
            case,
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            13,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;

    for (row, gateway_id, link_id, addressed_rule) in [
        (
            "a gateway that is no end of the link",
            other_pair.responder_id.as_str(),
            pair.link_id.as_str(),
            rule_id.as_str(),
        ),
        (
            "a link id nothing minted",
            pair.responder_id.as_str(),
            "link-nothing-minted",
            rule_id.as_str(),
        ),
        (
            "a rule id this link does not carry",
            pair.responder_id.as_str(),
            pair.link_id.as_str(),
            "rule-nothing-minted",
        ),
        (
            "a rule stored on another link",
            other_pair.responder_id.as_str(),
            other_pair.link_id.as_str(),
            rule_id.as_str(),
        ),
        (
            "a gateway id nothing minted",
            "rtb-gw-nothing",
            pair.link_id.as_str(),
            rule_id.as_str(),
        ),
    ] {
        let answered = fabric
            .update_rule(
                gateway_id,
                link_id,
                addressed_rule,
                14,
                json!({ "pathPrefix": "/bid" }),
            )
            .await;
        assert_not_found(row, &answered, &[gateway_id, link_id, addressed_rule]);
    }

    // The refused calls stored nothing against the rule that does exist.
    let read = fabric
        .read_rule(case, &pair.responder_id, &pair.link_id, &rule_id)
        .await;
    assert_eq!(
        read["priority"], 13,
        "case {case:?}: expected the stored rule to be untouched, got {:?}",
        read["priority"]
    );
}

#[tokio::test]
async fn a_rule_on_its_way_out_has_no_configuration_to_change() {
    let case = "a rule whose deletion is outstanding";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;
    let rule_id = fabric
        .routing_rule(
            case,
            &pair.responder_id,
            &pair.link_id,
            "token-rule",
            15,
            json!({ "pathPrefix": "/bid" }),
        )
        .await;

    let deleted = fabric
        .delete_rule(&pair.responder_id, &pair.link_id, &rule_id)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: expected DeleteLinkRoutingRule to answer 200, got {} {:?}",
        deleted.status, deleted.body
    );
    assert_eq!(
        text(&deleted.body, "status"),
        RULE_STATUS_DELETION_IN_PROGRESS,
        "case {case:?}: expected the delete to answer {RULE_STATUS_DELETION_IN_PROGRESS:?}, got {:?}",
        deleted.body["status"]
    );

    let refused = fabric
        .update_rule(
            &pair.responder_id,
            &pair.link_id,
            &rule_id,
            16,
            json!({ "pathExact": "/bid/openrtb" }),
        )
        .await;
    assert_eq!(
        refused.status, 409,
        "case {case:?}: expected HTTP 409, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.code, "ConflictException",
        "case {case:?}: expected ConflictException, got {:?}",
        refused.code
    );
    assert_eq!(
        refused.message(),
        format!(
            "rule {rule_id} on link {} is {RULE_STATUS_DELETION_IN_PROGRESS} and cannot be updated",
            pair.link_id
        ),
        "case {case:?}: expected the refusal to name the rule and the status refusing it, got {:?}",
        refused.message()
    );

    // Once the clock has completed the deletion the rule is gone, so the update is
    // the service's one not-found rather than a conflict.
    fabric
        .gone(case, &pair.responder_id, &pair.link_id, &rule_id)
        .await;
    let answered = fabric
        .update_rule(
            &pair.responder_id,
            &pair.link_id,
            &rule_id,
            16,
            json!({ "pathExact": "/bid/openrtb" }),
        )
        .await;
    assert_not_found(
        case,
        &answered,
        &[&pair.responder_id, &pair.link_id, &rule_id],
    );
}
