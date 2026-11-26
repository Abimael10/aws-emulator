// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for one rule of the `/tags/{resourceArn}` surface: a tagging call
//! answers about exactly the set of records the rest of the crate holds at that
//! instant.
//!
//! A routing rule's deletion is asynchronous — the record holds
//! `DELETION_IN_PROGRESS` until the clock makes the deletion due, and the store's
//! settling then drops it — so the tagging surface may not report, or write to, a
//! record the clock has already released: `ListTagsForResource`, `TagResource` and
//! `UntagResource` must each materialise the transitions the clock has made due before
//! they resolve the ARN, exactly as the resource's own read does.
//!
//! The table spans what the rule generalises over: which of the three operations on
//! the surface makes the FIRST call after the deletion comes due (the read and both
//! writes), and which of the link's two ends the ARN addresses the rule through. Each
//! row drives the tagging call BEFORE the rule's own read, so a surface that has not
//! settled is caught reporting a record that read answers not-found for, rather than
//! being handed an already-settled store.
//!
//! The last suite pins the other side of the rule: settling releases a routing rule
//! and nothing else, so the gateways and the link the rule hung off are still
//! addressable through the same surface afterwards.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::RULE_STATUS_DELETION_IN_PROGRESS;
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for, and so the region every ARN it builds names.
const REGION: &str = "us-west-2";

/// How long a wait for a clock-owed transition is given before the test gives up.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll loop pauses between two rounds of reads.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// The answer a tagging call naming no stored resource carries: the status, the wire
/// error code and the whole message, written out here rather than read back from the
/// crate under test, so a handler that moved to another refusal fails this suite. AWS
/// answers an unresolvable rtbfabric tagging ARN this way rather than with the
/// `ResourceNotFoundException` the documentation suggests.
const UNRESOLVED_STATUS: u16 = 500;
const UNRESOLVED_CODE: &str = "InternalServerErrorException";
const UNRESOLVED_MESSAGE: &str = "Internal server error.";

/// One answered call: the status, the wire error code (absent on a success) and the
/// parsed body.
struct Answer {
    status: u16,
    code: String,
    body: Value,
}

impl Answer {
    /// Whether this answer is the refusal the surface gives for an ARN naming no
    /// record the store holds.
    fn unresolved(&self) -> bool {
        self.status == UNRESOLVED_STATUS
    }
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

/// Percent-encode a value the way an AWS SDK serializes a non-greedy `@httpLabel` or
/// an `@httpQuery` value, so an ARN's `:` and `/` arrive as ONE path segment.
fn encoded(value: &str) -> String {
    utf8_percent_encode(value, NON_ALPHANUMERIC).to_string()
}

/// The `/tags/{resourceArn}` path for `resource_arn`.
fn tags_path(resource_arn: &str) -> String {
    format!("/tags/{}", encoded(resource_arn))
}

/// The `UntagResource` path: the resource in the label, the keys in the repeated
/// `tagKeys` query parameter the model binds.
fn untag_path(resource_arn: &str, tag_keys: &[&str]) -> String {
    let query: Vec<String> = tag_keys
        .iter()
        .map(|key| format!("tagKeys={}", encoded(key)))
        .collect();
    format!("{}?{}", tags_path(resource_arn), query.join("&"))
}

/// The ARN of a resource of this service, in the account and Region this emulator
/// serves.
fn resource_arn(resource: &str) -> String {
    ::engine::emulator_arn("rtbfabric", REGION, resource)
}

/// A requester gateway, a responder gateway and the link between them.
struct Pair {
    requester_id: String,
    responder_id: String,
    link_id: String,
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate -> handle ->
/// serialize path.
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
    async fn linked_pair(&self, case: &str) -> Pair {
        let requester = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": "token-requester",
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
                    "clientToken": "token-responder",
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

    /// Create a routing rule carrying one tag, and answer the id the service minted.
    async fn create_rule(&self, case: &str, pair: &Pair) -> String {
        let created = self
            .send(
                "POST",
                &format!(
                    "/responder-gateway/{}/link/{}/routing-rule",
                    pair.responder_id, pair.link_id
                ),
                Some(json!({
                    "clientToken": "token-rule",
                    "priority": 1,
                    "conditions": { "pathPrefix": "/bid" },
                    "tags": { "tier": "gold" },
                })),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: expected CreateLinkRoutingRule to answer 200, got {} {:?}",
            created.status, created.body
        );
        text(&created.body, "ruleId")
    }

    /// Ask for the rule's deletion, which the service answers before the rule is gone.
    async fn delete_rule(&self, case: &str, pair: &Pair, rule_id: &str) {
        let deleted = self
            .send(
                "DELETE",
                &format!(
                    "/responder-gateway/{}/link/{}/routing-rule/{rule_id}",
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
        assert_eq!(
            text(&deleted.body, "status"),
            RULE_STATUS_DELETION_IN_PROGRESS,
            "case {case:?}: expected the delete to answer {RULE_STATUS_DELETION_IN_PROGRESS:?}, got {:?}",
            deleted.body["status"]
        );
    }

    /// The rule's own read, through the gateway the ARN addresses it by.
    async fn get_rule(&self, gateway_id: &str, link_id: &str, rule_id: &str) -> Answer {
        self.send(
            "GET",
            &format!("/responder-gateway/{gateway_id}/link/{link_id}/routing-rule/{rule_id}"),
            None,
        )
        .await
    }
}

/// Which operation of the surface makes the first call after the clock releases the
/// rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Surface {
    List,
    Tag,
    Untag,
}

impl Surface {
    /// Drive this operation against `arn`.
    async fn call(self, fabric: &Fabric, arn: &str) -> Answer {
        match self {
            Surface::List => fabric.send("GET", &tags_path(arn), None).await,
            Surface::Tag => {
                fabric
                    .send(
                        "POST",
                        &tags_path(arn),
                        Some(json!({ "tags": { "owner": "ads" } })),
                    )
                    .await
            }
            Surface::Untag => {
                fabric
                    .send("DELETE", &untag_path(arn, &["tier"]), None)
                    .await
            }
        }
    }

    /// The status this operation answers for a record the store holds.
    fn success_status(self) -> u16 {
        match self {
            // `ListTagsForResource` reports a body; both writes answer 204 with none.
            Surface::List => 200,
            Surface::Tag | Surface::Untag => 204,
        }
    }

    /// The `tags` member the rule's own read reports after ONE call of this operation
    /// on a rule the create gave the single tag `tier`.
    fn tags_after_the_call(self) -> Value {
        match self {
            Surface::List => json!({ "tier": "gold" }),
            Surface::Tag => json!({ "owner": "ads", "tier": "gold" }),
            // The removal names the one tag the create assigned, so the record holds
            // none afterwards and the read omits the member rather than reporting an
            // empty map.
            Surface::Untag => Value::Null,
        }
    }
}

/// Which end of the link the tagging ARN addresses the rule through: a rule is
/// addressable from either end, exactly as its link is.
#[derive(Debug, Clone, Copy)]
enum End {
    Responder,
    Requester,
}

/// One row of the table: the operation that calls first, and the end the ARN names.
struct Row {
    name: &'static str,
    surface: Surface,
    end: End,
}

const ROWS: &[Row] = &[
    Row {
        name: "ListTagsForResource through the responder end",
        surface: Surface::List,
        end: End::Responder,
    },
    Row {
        name: "ListTagsForResource through the requester end",
        surface: Surface::List,
        end: End::Requester,
    },
    Row {
        name: "TagResource through the responder end",
        surface: Surface::Tag,
        end: End::Responder,
    },
    Row {
        name: "TagResource through the requester end",
        surface: Surface::Tag,
        end: End::Requester,
    },
    Row {
        name: "UntagResource through the responder end",
        surface: Surface::Untag,
        end: End::Responder,
    },
    Row {
        name: "UntagResource through the requester end",
        surface: Surface::Untag,
        end: End::Requester,
    },
];

/// Assert `answered` is the surface's one refusal for an ARN naming no stored record.
fn assert_unresolved(case: &str, operation: &str, answered: &Answer) {
    assert_eq!(
        answered.status, UNRESOLVED_STATUS,
        "case {case:?}: expected {operation} to answer {UNRESOLVED_STATUS}, got {} {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.code, UNRESOLVED_CODE,
        "case {case:?}: expected {operation} to answer {UNRESOLVED_CODE}, got {:?}",
        answered.code
    );
    assert_eq!(
        text(&answered.body, "message"),
        UNRESOLVED_MESSAGE,
        "case {case:?}: expected {operation} to state {UNRESOLVED_MESSAGE:?}, got {:?}",
        answered.body
    );
}

#[tokio::test]
async fn the_tagging_surface_settles_the_clock_before_it_resolves_an_arn() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair(case).await;
        let rule_id = fabric.create_rule(case, &pair).await;
        let end = match row.end {
            End::Responder => pair.responder_id.clone(),
            End::Requester => pair.requester_id.clone(),
        };
        let arn = resource_arn(&format!(
            "gateway/{end}/link/{}/routing-rule/{rule_id}",
            pair.link_id
        ));

        // While the record IS held, this operation and the rule's own read answer
        // about the same record: what the surface reports or writes is on the record
        // the read reports.
        let held = row.surface.call(&fabric, &arn).await;
        assert_eq!(
            held.status,
            row.surface.success_status(),
            "case {case:?}: expected the tagging call on a stored rule to answer {}, got {} {:?}",
            row.surface.success_status(),
            held.status,
            held.body
        );
        let read = fabric
            .get_rule(&pair.responder_id, &pair.link_id, &rule_id)
            .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the rule read to answer 200 while the rule is held, got {} {:?}",
            read.status, read.body
        );
        assert_eq!(
            read.body["tags"],
            row.surface.tags_after_the_call(),
            "case {case:?}: expected the rule read to report the tags the tagging call left, got {:?}",
            read.body["tags"]
        );

        fabric.delete_rule(case, &pair, &rule_id).await;

        // Wait for the CONDITION the clock owes — the rule's release — by looking
        // again, never by sleeping for the span. ONLY this operation is called while
        // waiting: nothing else can settle the store for it, so a handler that leaves
        // the clock to somebody else keeps answering about the released record until
        // the deadline passes.
        let give_up_at = Instant::now() + SETTLE_DEADLINE;
        loop {
            let answered = row.surface.call(&fabric, &arn).await;
            if answered.unresolved() {
                assert_unresolved(case, "the tagging call", &answered);
                break;
            }
            assert_eq!(
                answered.status,
                row.surface.success_status(),
                "case {case:?}: expected the tagging call to answer {} or the unresolved refusal, got {} {:?}",
                row.surface.success_status(),
                answered.status,
                answered.body
            );
            assert!(
                Instant::now() < give_up_at,
                "case {case:?}: expected the tagging call to refuse a rule whose release the clock has completed, got {} {:?}",
                answered.status,
                answered.body
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }

        // The rule's own read says the same: the two surfaces report ONE set of rules.
        let read = fabric
            .get_rule(&pair.responder_id, &pair.link_id, &rule_id)
            .await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the rule read to answer 404 for the released rule, got {} {:?}",
            read.status, read.body
        );

        // And so does every other operation of the surface, whichever one asked first.
        for (operation, answered) in [
            ("ListTagsForResource", Surface::List.call(&fabric, &arn).await),
            ("TagResource", Surface::Tag.call(&fabric, &arn).await),
            ("UntagResource", Surface::Untag.call(&fabric, &arn).await),
        ] {
            assert_unresolved(case, operation, &answered);
        }

        // Settling releases the rule and nothing else: every other record this surface
        // addressed before is addressable still.
        for (kind, resource) in [
            (
                "the requester gateway",
                format!("gateway/{}", pair.requester_id),
            ),
            (
                "the responder gateway",
                format!("gateway/{}", pair.responder_id),
            ),
            (
                "the link the rule routed for",
                format!("gateway/{}/link/{}", pair.responder_id, pair.link_id),
            ),
        ] {
            let listed = fabric
                .send("GET", &tags_path(&resource_arn(&resource)), None)
                .await;
            assert_eq!(
                listed.status, 200,
                "case {case:?}: expected ListTagsForResource on {kind} to answer 200, got {} {:?}",
                listed.status, listed.body
            );
        }
    }
}
