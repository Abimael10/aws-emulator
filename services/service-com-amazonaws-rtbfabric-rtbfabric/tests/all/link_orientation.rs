// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the orientation every link response carries: a link is
//! ONE record with two ends, and a response addressed to one of them reports
//! that gateway as `gatewayId` and the link's OTHER end as `peerGatewayId`.
//!
//! The rule is about which end the call addressed, never about which end opened
//! the link, so the table spans what it generalises over: which kind of gateway
//! opened the link (a requester, which makes it a `REQUEST` link, and a
//! responder, which makes it a `RESPONSE` one), which of the two ends addresses
//! the read, and the status the link is in when it is read (the unsettled
//! request, the settled one, and the link an accept has brought up). Every
//! reader of the record is driven on every row — `GetLink`, `ListLinks`, and the
//! `AcceptLink`/`RejectLink` answers to the same link — so the single-link
//! reader, the listing and the two answers never disagree about which gateway is
//! the peer.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    LINK_STATUS_ACTIVE, LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REJECTED, LINK_STATUS_REQUESTED,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// How long a poll waits for a status the clock owes before it gives up. Far
/// above any span this service owes a transition, so a slow or loaded host
/// changes how many times the loop looks, never whether it succeeds.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The members that carry a link's orientation, from
/// `smithy_query(query="operation", operation="GetLink")`: both are required
/// members of every link response shape.
const ORIENTATION_MEMBERS: &[&str] = &["gatewayId", "peerGatewayId"];

/// `LinkLogSettings` with its one required member, as
/// `smithy_query(query="shape", shape="LinkLogSettings")` declares it.
fn log_settings() -> Value {
    json!({ "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } } })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

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

    /// Send one restJson1 request and return `(status, parsed body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
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
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// A requester gateway and a responder gateway, both over the wire, as
    /// `(the requester's id, the responder's id)`.
    async fn gateway_pair(&self, case: &str, token: &str) -> (String, String) {
        let (status, requester) = self
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
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {requester:?}"
        );
        let (status, responder) = self
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
            status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {status} {responder:?}"
        );
        (text(&requester, "gatewayId"), text(&responder, "gatewayId"))
    }

    /// Open a link from `opener_id` to `peer_id` over the wire, and answer with
    /// the link id. Either kind of gateway can be the opener.
    async fn open_link(&self, case: &str, opener_id: &str, peer_id: &str) -> String {
        let (status, link) = self
            .send(
                "POST",
                &format!("/gateway/{opener_id}/create-link"),
                Some(json!({
                    "peerGatewayId": peer_id,
                    "logSettings": log_settings(),
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateLink to answer 200, got {status} {link:?}"
        );
        text(&link, "linkId")
    }

    /// `GetLink` for one link, addressed to `gateway_id`.
    async fn read_link(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let (status, body) = self
            .send("GET", &format!("/gateway/{gateway_id}/link/{link_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetLink on gateway {gateway_id:?} to answer 200, got {status} {body:?}"
        );
        body
    }

    /// The `ListLinks` entry for one link, addressed to `gateway_id`.
    async fn listed_link(&self, case: &str, gateway_id: &str, link_id: &str) -> Value {
        let (status, body) = self
            .send("GET", &format!("/gateway/{gateway_id}/links/"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListLinks on gateway {gateway_id:?} to answer 200, got {status} {body:?}"
        );
        let entries = body["links"].as_array().cloned().unwrap_or_default();
        entries
            .into_iter()
            .find(|entry| text(entry, "linkId") == link_id)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected gateway {gateway_id:?} to list link {link_id:?}, got {body:?}")
            })
    }

    /// Read the link through `GetLink` until it reports `expected`, and answer
    /// with that reading.
    ///
    /// A settled status is owed by the clock, so the test waits for the
    /// CONDITION rather than for a span: it looks again until the status holds,
    /// giving up only once [`POLL_DEADLINE`] has passed.
    async fn poll_link_status(
        &self,
        case: &str,
        gateway_id: &str,
        link_id: &str,
        expected: &str,
    ) -> Value {
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let body = self.read_link(case, gateway_id, link_id).await;
            if text(&body, "status") == expected {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected link {link_id:?} to reach {expected:?}, got {:?}",
                body["status"]
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}

/// How far the link has been taken before it is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// The request as `CreateLink` left it, before the clock settled it.
    RequestOutstanding,
    /// The request after the clock put it to the peer.
    RequestSettled,
    /// The link the peer accepted, once it is carrying traffic.
    Accepted,
}

/// One row: which kind of gateway opened the link, and how far the link has been
/// taken when every reader describes it.
#[derive(Debug)]
struct Row {
    name: &'static str,
    /// `true` when the requester gateway opened the link (a `REQUEST` link),
    /// `false` when the responder gateway opened it (a `RESPONSE` one).
    opened_by_requester: bool,
    stage: Stage,
}

const ROWS: &[Row] = &[
    Row {
        name: "a requester-opened request, unsettled",
        opened_by_requester: true,
        stage: Stage::RequestOutstanding,
    },
    Row {
        name: "a requester-opened request, settled",
        opened_by_requester: true,
        stage: Stage::RequestSettled,
    },
    Row {
        name: "a requester-opened link the peer accepted",
        opened_by_requester: true,
        stage: Stage::Accepted,
    },
    Row {
        name: "a responder-opened request, unsettled",
        opened_by_requester: false,
        stage: Stage::RequestOutstanding,
    },
    Row {
        name: "a responder-opened request, settled",
        opened_by_requester: false,
        stage: Stage::RequestSettled,
    },
    Row {
        name: "a responder-opened link the peer accepted",
        opened_by_requester: false,
        stage: Stage::Accepted,
    },
];

/// Assert one link response describes the link as addressed from `addressed`,
/// whose other end is `other`.
fn assert_as_addressed(case: &str, reader: &str, body: &Value, addressed: &str, other: &str) {
    assert_eq!(
        text(body, "gatewayId").as_str(),
        addressed,
        "case {case:?}: expected {reader} to report the addressed gateway {addressed:?} as gatewayId, got {:?}",
        body["gatewayId"]
    );
    assert_eq!(
        text(body, "peerGatewayId").as_str(),
        other,
        "case {case:?}: expected {reader} to report the other end {other:?} as peerGatewayId, got {:?}",
        body["peerGatewayId"]
    );
    assert_ne!(
        text(body, "gatewayId"),
        text(body, "peerGatewayId"),
        "case {case:?}: expected {reader} to name two different ends, got {body:?}"
    );
}

#[tokio::test]
async fn every_link_reader_reports_the_addressed_end_and_its_peer() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let (requester_id, responder_id) = fabric.gateway_pair(case, "orientation").await;
        let (opener_id, peer_id) = if row.opened_by_requester {
            (requester_id.clone(), responder_id.clone())
        } else {
            (responder_id.clone(), requester_id.clone())
        };
        let link_id = fabric.open_link(case, &opener_id, &peer_id).await;

        let expected_status = match row.stage {
            Stage::RequestOutstanding => LINK_STATUS_PENDING_REQUEST,
            Stage::RequestSettled => {
                fabric
                    .poll_link_status(case, &opener_id, &link_id, LINK_STATUS_REQUESTED)
                    .await;
                LINK_STATUS_REQUESTED
            }
            Stage::Accepted => {
                let (status, accepted) = fabric
                    .send(
                        "POST",
                        &format!("/gateway/{peer_id}/link/{link_id}/accept"),
                        Some(json!({ "logSettings": log_settings() })),
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected AcceptLink to answer 200, got {status} {accepted:?}"
                );
                // The accept is addressed to the peer end, so its own answer is
                // the rule's other orientation.
                assert_as_addressed(case, "AcceptLink", &accepted, &peer_id, &opener_id);
                fabric
                    .poll_link_status(case, &peer_id, &link_id, LINK_STATUS_ACTIVE)
                    .await;
                LINK_STATUS_ACTIVE
            }
        };

        // Both ends read and list the one record, each getting ITSELF as
        // `gatewayId` and the other end as its peer.
        for (addressed, other) in [(&opener_id, &peer_id), (&peer_id, &opener_id)] {
            let reading = fabric.read_link(case, addressed, &link_id).await;
            assert_eq!(
                text(&reading, "status"),
                expected_status,
                "case {case:?}: expected the link read from {addressed:?} to be {expected_status:?}, got {:?}",
                reading["status"]
            );
            assert_as_addressed(case, "GetLink", &reading, addressed, other);

            let listed = fabric.listed_link(case, addressed, &link_id).await;
            assert_as_addressed(case, "ListLinks", &listed, addressed, other);
            for member in ORIENTATION_MEMBERS {
                assert_eq!(
                    listed[member], reading[member],
                    "case {case:?}: expected the listed {member:?} to be what GetLink reports ({:?}), got {:?}",
                    reading[member], listed[member]
                );
            }
        }

        // One record, two ends: the two readings name the same pair of gateways
        // in opposite order, so neither end is ever its own peer.
        let from_opener = fabric.read_link(case, &opener_id, &link_id).await;
        let from_peer = fabric.read_link(case, &peer_id, &link_id).await;
        assert_eq!(
            text(&from_opener, "gatewayId"),
            text(&from_peer, "peerGatewayId"),
            "case {case:?}: expected the two readings to describe one record with two ends, got {from_opener:?} and {from_peer:?}"
        );
        assert_eq!(
            text(&from_opener, "peerGatewayId"),
            text(&from_peer, "gatewayId"),
            "case {case:?}: expected the two readings to describe one record with two ends, got {from_opener:?} and {from_peer:?}"
        );
    }
}

#[tokio::test]
async fn a_declined_link_is_described_as_addressed_by_the_reader_too() {
    // The decline is answered from the end that addressed it; the reader of the
    // same record from the OTHER end reports the opposite orientation, which is
    // the rule, not the record's stored direction.
    for from_peer in [true, false] {
        let case = if from_peer {
            "the peer declines the request"
        } else {
            "the opener withdraws the request"
        };
        let fabric = Fabric::new();
        let (requester_id, responder_id) = fabric.gateway_pair(case, "declined").await;
        let link_id = fabric.open_link(case, &requester_id, &responder_id).await;
        let (addressed, other) = if from_peer {
            (&responder_id, &requester_id)
        } else {
            (&requester_id, &responder_id)
        };

        let (status, rejected) = fabric
            .send(
                "POST",
                &format!("/gateway/{addressed}/link/{link_id}/reject"),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected RejectLink to answer 200, got {status} {rejected:?}"
        );
        assert_as_addressed(case, "RejectLink", &rejected, addressed, other);

        let reading = fabric.read_link(case, addressed, &link_id).await;
        assert_eq!(
            text(&reading, "status"),
            LINK_STATUS_REJECTED,
            "case {case:?}: expected the declined link to be REJECTED, got {:?}",
            reading["status"]
        );
        assert_as_addressed(case, "GetLink", &reading, addressed, other);
        let listed = fabric.listed_link(case, addressed, &link_id).await;
        assert_as_addressed(case, "ListLinks", &listed, addressed, other);

        let from_other = fabric.read_link(case, other, &link_id).await;
        assert_as_addressed(case, "GetLink", &from_other, other, addressed);
    }
}
