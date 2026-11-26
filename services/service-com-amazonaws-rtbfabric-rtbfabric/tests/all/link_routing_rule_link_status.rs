// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the link status `CreateLinkRoutingRule` stands on: a rule
//! routes traffic for one link, so whether the link can still take configuration at
//! all is decided before any rule is stored for it.
//!
//! The rule under test is about state, not about one recorded request: a link that
//! has reached a status nothing moves it out of has no configuration left to take,
//! so a call that configures such a link — a rule that routes for it included — is
//! refused with the modelled `ConflictException` (HTTP 409) and stores nothing,
//! while a link in every other status takes the rule and reports it back. The
//! refusal is the one `UpdateLink` and `UpdateLinkModuleFlow` already word for the
//! same question, naming the link and the gateway the call addressed and the status
//! that refuses it.
//!
//! The tables span what the rule generalises over: every member of the modelled
//! `LinkStatus` enum (`smithy_query(query="shape", shape="LinkStatus")`, read here
//! from this crate's `LINK_STATUS_VALUES` catalog), which of the link's two ends
//! addresses the call, and which condition and priority the refused call asks for —
//! the refusal is about the link, so what the rule would have matched on cannot
//! change it. One case drives the whole thing over the wire instead, declining the
//! link request through `RejectLink` rather than putting the status on the record.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::LINK_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    ComAmazonawsRtbfabricRtbfabricState, LINK_STATUS_DELETED, LINK_STATUS_FAILED,
    LINK_STATUS_REJECTED, RULE_STATUS_CREATION_IN_PROGRESS,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

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

impl Pair {
    /// The link's two ends, as the id a call addresses it through: either end may
    /// create a rule on the link, exactly as either may read it.
    fn end(&self, from_peer: bool) -> &str {
        if from_peer {
            &self.responder_id
        } else {
            &self.requester_id
        }
    }
}

/// One rule a refused call could have asked for: the refusal stands on the link's
/// status, so neither the priority nor the condition it names changes the answer.
struct RuleAsked {
    name: &'static str,
    priority: i64,
    conditions: fn() -> Value,
}

/// Rows differing along both members a create states about the rule itself, each at
/// a published bound of the modelled `RulePriority` range (1..1000) and on a
/// different member of the modelled `RuleCondition`
/// (`smithy_query(query="shape", shape="RuleCondition,RulePriority")`).
const RULES_ASKED: &[RuleAsked] = &[
    RuleAsked {
        name: "the first priority matching a path prefix",
        priority: 1,
        conditions: || json!({ "pathPrefix": "/bid" }),
    },
    RuleAsked {
        name: "the last priority matching a host header",
        priority: 1000,
        conditions: || json!({ "hostHeader": "bids.example.com" }),
    },
];

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so every call takes the real deserialize -> validate -> handle
/// -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
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

    /// A gateway pair with a link opened from the requester to the responder, all
    /// over the wire.
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
        asked: &RuleAsked,
    ) -> Answer {
        self.send(
            "POST",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule"),
            Some(json!({
                "clientToken": token,
                "priority": asked.priority,
                "conditions": (asked.conditions)(),
            })),
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

    /// Every rule the listing reports for one link.
    async fn list_rules(&self, case: &str, gateway_id: &str, link_id: &str) -> Vec<Value> {
        let answered = self
            .send(
                "GET",
                &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rules"),
                None,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected ListLinkRoutingRules to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.body["rules"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// Read one link, asserting the read itself answered.
    async fn link(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let answered = self
            .send("GET", &format!("/gateway/{gateway_id}/link/{link_id}"), None)
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected GetLink to answer 200, got {} {:?}",
            answered.status, answered.body
        );
        answered.body
    }

    /// Put `status` on every stored link, as `link_rejection.rs` does: the statuses
    /// a link reaches beyond its own request are driven by operations this crate has
    /// not implemented yet, so the state is set directly to cover the whole modelled
    /// enum rather than only the part the wire reaches today.
    fn force_link_status(&self, status: &str) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let mut store = state.lock();
        for link in store.links.values_mut() {
            link.status = status.to_string();
            link.settles_at = None;
        }
    }
}

/// The three members of the modelled `LinkStatus` enum nothing moves a link out of.
const TERMINAL_STATUSES: &[&str] = &[
    LINK_STATUS_REJECTED,
    LINK_STATUS_FAILED,
    LINK_STATUS_DELETED,
];

/// Every other member of that enum: the statuses a link can still be configured
/// from, taken from the catalog so a member added to the model is covered here.
fn configurable_link_statuses() -> Vec<&'static str> {
    LINK_STATUS_VALUES
        .iter()
        .copied()
        .filter(|status| !TERMINAL_STATUSES.contains(status))
        .collect()
}

#[tokio::test]
async fn a_rule_is_refused_for_a_link_in_a_terminal_status_and_nothing_is_stored() {
    // Spans what the rule generalises over: each status nothing moves a link out
    // of, which of the link's two ends addresses the call, and what the call asks
    // the rule to be.
    for status_value in TERMINAL_STATUSES {
        for from_peer in [false, true] {
            for asked in RULES_ASKED {
                let case = format!(
                    "{status_value} addressed from the {} end, {}",
                    if from_peer { "answering" } else { "opening" },
                    asked.name
                );
                let case = case.as_str();
                let fabric = Fabric::new();
                let pair = fabric.linked_pair(case, "token").await;
                fabric.force_link_status(status_value);
                let addressed = pair.end(from_peer);
                let before = fabric.link(case, addressed, &pair.link_id).await;

                let refused = fabric
                    .create_rule(addressed, &pair.link_id, "rule-token", asked)
                    .await;
                assert_eq!(
                    refused.status, 409,
                    "case {case:?}: expected a link in a status nothing leaves to conflict with 409, got {} {:?}",
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
                        "link {} on gateway {addressed} is {status_value} and cannot be routed for",
                        pair.link_id
                    ),
                    "case {case:?}: expected the refusal to name the link, the gateway and the status, got {:?}",
                    refused.message()
                );

                // Nothing was stored: the link carries no rule, through either of
                // its ends, and the link record itself is untouched.
                for reader in [pair.end(false), pair.end(true)] {
                    let listed = fabric.list_rules(case, reader, &pair.link_id).await;
                    assert!(
                        listed.is_empty(),
                        "case {case:?}: expected the refused create to store no rule, got {listed:?} through {reader}"
                    );
                }
                let after = fabric.link(case, addressed, &pair.link_id).await;
                assert_eq!(
                    after, before,
                    "case {case:?}: expected the refused call to change nothing, got {after:?} after {before:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_rule_is_stored_for_a_link_in_every_status_that_still_takes_configuration() {
    // The same table, over the rest of the modelled enum: the refusal is for the
    // terminal statuses only, so every other status takes the rule and reads it
    // back.
    for status_value in configurable_link_statuses() {
        for from_peer in [false, true] {
            let case = format!(
                "{status_value} addressed from the {} end",
                if from_peer { "answering" } else { "opening" }
            );
            let case = case.as_str();
            let fabric = Fabric::new();
            let pair = fabric.linked_pair(case, "token").await;
            fabric.force_link_status(status_value);
            let addressed = pair.end(from_peer);
            let asked = &RULES_ASKED[0];

            let created = fabric
                .create_rule(addressed, &pair.link_id, "rule-token", asked)
                .await;
            assert_eq!(
                created.status, 200,
                "case {case:?}: expected a link that still takes configuration to be served, got {} {:?}",
                created.status, created.body
            );
            assert_eq!(
                text(&created.body, "status"),
                RULE_STATUS_CREATION_IN_PROGRESS,
                "case {case:?}: expected a created rule to report {RULE_STATUS_CREATION_IN_PROGRESS:?}, got {:?}",
                created.body["status"]
            );

            // Round trip: the rule the create minted is the rule the reader reports.
            let rule_id = text(&created.body, "ruleId");
            let read_back = fabric.get_rule(addressed, &pair.link_id, &rule_id).await;
            assert_eq!(
                read_back.status, 200,
                "case {case:?}: expected GetLinkRoutingRule to answer 200, got {} {:?}",
                read_back.status, read_back.body
            );
            assert_eq!(
                text(&read_back.body, "ruleId"),
                rule_id,
                "case {case:?}: expected rule {rule_id:?} to read back, got {:?}",
                read_back.body["ruleId"]
            );
            assert_eq!(
                read_back.body["priority"],
                json!(asked.priority),
                "case {case:?}: expected priority {}, got {:?}",
                asked.priority,
                read_back.body["priority"]
            );
            assert_eq!(
                read_back.body["conditions"],
                (asked.conditions)(),
                "case {case:?}: expected the stored conditions {:?}, got {:?}",
                (asked.conditions)(),
                read_back.body["conditions"]
            );
        }
    }
}

#[tokio::test]
async fn a_rule_is_refused_for_a_link_the_peer_declined_over_the_wire() {
    // The same refusal, with the terminal status reached the way a caller reaches
    // it: the end the request was put to declines it, and the link is finished.
    let case = "the peer declined the link request";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;
    let rejected = fabric
        .send(
            "POST",
            &format!(
                "/gateway/{}/link/{}/reject",
                pair.responder_id, pair.link_id
            ),
            None,
        )
        .await;
    assert_eq!(
        rejected.status, 200,
        "case {case:?}: expected RejectLink to answer 200, got {} {:?}",
        rejected.status, rejected.body
    );
    assert_eq!(
        text(&rejected.body, "status"),
        LINK_STATUS_REJECTED,
        "case {case:?}: expected the declined link to be {LINK_STATUS_REJECTED:?}, got {:?}",
        rejected.body["status"]
    );

    let refused = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "rule-token",
            &RULES_ASKED[0],
        )
        .await;
    assert_eq!(
        refused.status, 409,
        "case {case:?}: expected the declined link to conflict with 409, got {} {:?}",
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
            "link {} on gateway {} is {LINK_STATUS_REJECTED} and cannot be routed for",
            pair.link_id, pair.responder_id
        ),
        "case {case:?}: expected the refusal to name the link, the gateway and the status, got {:?}",
        refused.message()
    );
    let listed = fabric
        .list_rules(case, &pair.responder_id, &pair.link_id)
        .await;
    assert!(
        listed.is_empty(),
        "case {case:?}: expected the refused create to store no rule, got {listed:?}"
    );
}

#[tokio::test]
async fn a_retry_of_an_accepted_create_replays_its_rule_after_the_link_is_declined() {
    // The client token is what an accepted create is answered from, so a retry of
    // THAT call reports the rule it already minted rather than the refusal a fresh
    // create now meets.
    let case = "a retried create, then a fresh one";
    let fabric = Fabric::new();
    let pair = fabric.linked_pair(case, "token").await;
    let asked = &RULES_ASKED[0];
    let created = fabric
        .create_rule(&pair.responder_id, &pair.link_id, "rule-token", asked)
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected the create on a live link to answer 200, got {} {:?}",
        created.status, created.body
    );

    let rejected = fabric
        .send(
            "POST",
            &format!(
                "/gateway/{}/link/{}/reject",
                pair.responder_id, pair.link_id
            ),
            None,
        )
        .await;
    assert_eq!(
        rejected.status, 200,
        "case {case:?}: expected RejectLink to answer 200, got {} {:?}",
        rejected.status, rejected.body
    );

    let replayed = fabric
        .create_rule(&pair.responder_id, &pair.link_id, "rule-token", asked)
        .await;
    assert_eq!(
        replayed.status, 200,
        "case {case:?}: expected the retried create to be answered from its record, got {} {:?}",
        replayed.status, replayed.body
    );
    assert_eq!(
        text(&replayed.body, "ruleId"),
        text(&created.body, "ruleId"),
        "case {case:?}: expected the retry to report rule {:?}, got {:?}",
        created.body["ruleId"],
        replayed.body["ruleId"]
    );

    // A create that is not that retry is refused: the link is finished.
    let refused = fabric
        .create_rule(
            &pair.responder_id,
            &pair.link_id,
            "another-token",
            &RULES_ASKED[1],
        )
        .await;
    assert_eq!(
        refused.status, 409,
        "case {case:?}: expected a fresh create on the declined link to conflict with 409, got {} {:?}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.code, "ConflictException",
        "case {case:?}: expected ConflictException, got {:?}",
        refused.code
    );
}
