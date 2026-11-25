// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two answers to an outstanding link request:
//! `RejectLink` declines it, `AcceptLink` accepts it.
//!
//! The rules under test are about state: a link whose request is still
//! outstanding moves to the terminal `REJECTED` status on a decline, and on an
//! accept to `ACCEPTED` and then to the `ACTIVE` the clock owes it, each status
//! being what every later reader reports; a link in any other status cannot be
//! answered at all, beyond a repeat of the call that already put it there. The
//! tables span what the rules generalise over — which of the link's two ends
//! addresses the decline, whether the request has settled yet, which of the
//! accept's optional members the call states, and every member of the modelled
//! `LinkStatus` enum. The accept is addressed to the end the request was put
//! to, because that is the only end whose acceptance establishes the link;
//! which end may accept at all is covered in `link_accept_end.rs`.
//!
//! The decline's own consequence is covered here too: a link that has reached a
//! terminal status occupies the gateway pair no longer, so the two gateways can
//! be linked again.
//!
//! `UpdateLink` is covered here as well, because it is what happens next to a
//! link an accept has brought up: it replaces the configuration members the
//! call states, keeps every member it leaves out, and reports the status that
//! change leaves the link in. A call that states NEITHER of its two optional
//! members asks for no change, so it leaves the record exactly as it stands.
//! Its table spans which of the two optional members
//! the call states, the published bounds of the modelled timeout range, which
//! end addresses the call, and every member of the modelled `LinkStatus` enum.
//!
//! The counters the two gateway readers DERIVE from those statuses are covered
//! here too, because they are a function of exactly what this suite drives: a
//! link counts as active for both its ends while it carries traffic, the live
//! link under an update included, and counts as requested only for the end an
//! outstanding request is waiting on — the end the link was not opened from.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::LINK_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    ComAmazonawsRtbfabricRtbfabricState, LINK_STATUS_ACCEPTED, LINK_STATUS_ACTIVE,
    LINK_STATUS_DELETED, LINK_STATUS_FAILED, LINK_STATUS_PENDING_REQUEST,
    LINK_STATUS_PENDING_UPDATE, LINK_STATUS_REJECTED, LINK_STATUS_REQUESTED,
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
    engine: Arc<engine::InMemoryEngine>,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one restJson1 request and return `(status, parsed body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (status, _, answered) = self.send_with_code(method, path, body).await;
        (status, answered)
    }

    /// [`send`](Self::send), with the wire error code from `x-amzn-errortype`: a
    /// refusal is a status AND a code, and a caller's SDK dispatches on the code.
    async fn send_with_code(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, String, Value) {
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
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, code, parsed)
    }

    /// Read the link through `GetLink` until it reports `expected`, and answer
    /// with that reading.
    ///
    /// A settled status is owed by the clock, so the test waits for the
    /// CONDITION rather than for a span: it looks again until the status holds,
    /// giving up only once [`POLL_DEADLINE`] has passed.
    async fn poll_link_status(&self, gateway_id: &str, link_id: &str, expected: &str) -> Value {
        let path = format!("/gateway/{gateway_id}/link/{link_id}");
        let deadline = Instant::now() + POLL_DEADLINE;
        loop {
            let (status, body) = self.send("GET", &path, None).await;
            assert_eq!(
                status, 200,
                "expected GetLink on {path:?} to answer 200, got {status} {body:?}"
            );
            if text(&body, "status") == expected {
                return body;
            }
            assert!(
                Instant::now() < deadline,
                "expected {path:?} to reach {expected:?}, got {:?}",
                body["status"]
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Whether the clock still owes any stored link a status transition.
    fn settlement_owed(&self) -> bool {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let store = state.lock();
        let owed = store.links.values().any(|link| link.settles_at.is_some());
        owed
    }

    /// A requester gateway and a responder gateway, both over the wire, as
    /// `(the requester's id, the responder's id)`.
    async fn gateway_pair(&self, token: &str) -> (String, String) {
        let gateway = json!({
            "vpcId": "vpc-0123456789abcdef0",
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "clientToken": format!("{token}-requester"),
        });
        let (status, requester) = self.send("POST", "/requester-gateway", Some(gateway)).await;
        assert_eq!(status, 200, "CreateRequesterGateway: {status} {requester:?}");
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
        assert_eq!(status, 200, "CreateResponderGateway: {status} {responder:?}");
        (text(&requester, "gatewayId"), text(&responder, "gatewayId"))
    }

    /// Open a link from `opener_id` to `peer_id` over the wire, and answer with
    /// what `CreateLink` reported. Either kind of gateway can be the opener; the
    /// service reads the link's direction from which kind that is.
    async fn open_link(&self, opener_id: &str, peer_id: &str) -> Value {
        let (status, link) = self
            .send(
                "POST",
                &format!("/gateway/{opener_id}/create-link"),
                Some(json!({
                    "peerGatewayId": peer_id,
                    "logSettings": log_settings(),
                    "attributes": { "customerProvidedId": "spot-fabric" },
                })),
            )
            .await;
        assert_eq!(status, 200, "CreateLink: {status} {link:?}");
        link
    }

    /// A requester gateway, a responder gateway and a link opened from the
    /// requester to the responder, all over the wire.
    async fn linked_pair(&self, token: &str) -> Pair {
        let (requester_id, responder_id) = self.gateway_pair(token).await;
        let link = self.open_link(&requester_id, &responder_id).await;
        Pair {
            requester_id,
            responder_id,
            link_id: text(&link, "linkId"),
            created_at: link["createdAt"].clone(),
        }
    }

    /// Read one gateway, asserting the read itself answered.
    async fn gateway(&self, case: &str, path: &str) -> Value {
        let (status, body) = self.send("GET", path, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the read of {path:?} to answer 200, got {status} {body:?}"
        );
        body
    }

    /// Put `status` on every stored link.
    ///
    /// The statuses a link reaches beyond its own request are driven by
    /// operations this crate has not implemented yet, so the state is set
    /// directly to cover the whole modelled enum rather than only the part the
    /// wire reaches today.
    fn force_link_status(&self, status: &str) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let mut store = state.lock();
        for link in store.links.values_mut() {
            link.status = status.to_string();
            link.settles_at = None;
        }
    }
}

/// The gateway pair and the link between them, as the service minted them.
struct Pair {
    requester_id: String,
    responder_id: String,
    link_id: String,
    /// What `CreateLink` reported for `createdAt`; a reject must not change it.
    created_at: Value,
}

/// One row: which end addresses the reject, and whether the clock has settled
/// `PENDING_REQUEST` -> `REQUESTED` first, so both outstanding statuses are
/// covered from both ends.
struct Row {
    name: &'static str,
    /// `true` for the gateway the request was put to (the end the operation's
    /// documentation describes as declining it), `false` for the opener.
    from_peer: bool,
    settle_first: bool,
}

const ROWS: &[Row] = &[
    Row {
        name: "the peer declines an unsettled request",
        from_peer: true,
        settle_first: false,
    },
    Row {
        name: "the peer declines a settled request",
        from_peer: true,
        settle_first: true,
    },
    Row {
        name: "the opener withdraws an unsettled request",
        from_peer: false,
        settle_first: false,
    },
    Row {
        name: "the opener withdraws a settled request",
        from_peer: false,
        settle_first: true,
    },
];

#[tokio::test]
async fn rejecting_an_outstanding_request_moves_the_link_to_rejected_for_every_reader() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        let (addressed, other) = if row.from_peer {
            (&pair.responder_id, &pair.requester_id)
        } else {
            (&pair.requester_id, &pair.responder_id)
        };

        let expected_before = if row.settle_first {
            // The move to REQUESTED is owed by the clock: poll for it rather
            // than wait a span.
            let settled = fabric
                .poll_link_status(addressed, &pair.link_id, LINK_STATUS_REQUESTED)
                .await;
            assert_eq!(
                text(&settled, "status"),
                LINK_STATUS_REQUESTED,
                "case {case:?}: expected REQUESTED before the reject, got {:?}",
                settled["status"]
            );
            LINK_STATUS_REQUESTED
        } else {
            LINK_STATUS_PENDING_REQUEST
        };
        let (status, before) = fabric
            .send("GET", &format!("/gateway/{addressed}/link/{}", pair.link_id), None)
            .await;
        assert_eq!(
            text(&before, "status"),
            expected_before,
            "case {case:?}: expected {expected_before:?} before the reject, got {status} {:?}",
            before["status"]
        );

        let reject = format!("/gateway/{addressed}/link/{}/reject", pair.link_id);
        let (status, rejected) = fabric.send("POST", &reject, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected RejectLink to answer 200, got {status} {rejected:?}"
        );
        assert_eq!(
            text(&rejected, "status"),
            LINK_STATUS_REJECTED,
            "case {case:?}: expected the declined link to be REJECTED, got {:?}",
            rejected["status"]
        );
        assert_eq!(
            text(&rejected, "linkId"),
            pair.link_id,
            "case {case:?}: expected link {:?}, got {:?}",
            pair.link_id,
            rejected["linkId"]
        );
        assert_eq!(
            text(&rejected, "gatewayId").as_str(),
            addressed.as_str(),
            "case {case:?}: expected the addressed gateway {addressed:?} as gatewayId, got {:?}",
            rejected["gatewayId"]
        );
        assert_eq!(
            text(&rejected, "peerGatewayId").as_str(),
            other.as_str(),
            "case {case:?}: expected the other end {other:?} as peerGatewayId, got {:?}",
            rejected["peerGatewayId"]
        );
        // The link's own facts come out of the record, unchanged by the reject:
        // only its status and `updatedAt` move.
        for (member, expected) in [
            ("createdAt", pair.created_at.clone()),
            ("direction", json!("REQUEST")),
            ("connectivityType", json!("DEFAULT")),
            ("logSettings", log_settings()),
            ("attributes", json!({ "customerProvidedId": "spot-fabric" })),
        ] {
            assert_eq!(
                rejected[member], expected,
                "case {case:?}: expected the stored {member} {expected:?}, got {:?}",
                rejected[member]
            );
        }

        // A repeat of this `@idempotent` call asks for the state the link is
        // already in, so it answers the record the first reject left.
        let (status, repeat) = fabric.send("POST", &reject, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a repeated RejectLink to answer 200, got {status} {repeat:?}"
        );
        assert_eq!(
            repeat, rejected,
            "case {case:?}: expected the repeat to answer the first reject's record, got {repeat:?}"
        );

        // REJECTED is terminal and persisted: nothing further is owed on the
        // link, so the clock never moves it on, and both ends read it back.
        assert!(
            !fabric.settlement_owed(),
            "case {case:?}: expected nothing further to be owed on a REJECTED link"
        );
        for (end, gateway_id) in [("addressed end", addressed), ("other end", other)] {
            let (status, read_back) = fabric
                .send("GET", &format!("/gateway/{gateway_id}/link/{}", pair.link_id), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetLink from the {end} to answer 200, got {status} {read_back:?}"
            );
            assert_eq!(
                text(&read_back, "status"),
                LINK_STATUS_REJECTED,
                "case {case:?}: expected the {end} to read REJECTED back, got {:?}",
                read_back["status"]
            );
            assert_eq!(
                read_back["updatedAt"], rejected["updatedAt"],
                "case {case:?}: expected the {end} to read the reject's updatedAt {:?}, got {:?}",
                rejected["updatedAt"], read_back["updatedAt"]
            );
        }
    }
}

#[tokio::test]
async fn only_an_outstanding_or_an_already_declined_request_can_be_rejected() {
    // The whole modelled catalog, from `LINK_STATUS_VALUES` (this service's
    // `LinkStatus` enum), so a status no operation reaches yet is covered too.
    for status_value in LINK_STATUS_VALUES {
        let case = *status_value;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        fabric.force_link_status(status_value);
        let link = format!("/gateway/{}/link/{}", pair.responder_id, pair.link_id);

        let (status, body) = fabric.send("POST", &format!("{link}/reject"), None).await;

        if [
            LINK_STATUS_PENDING_REQUEST,
            LINK_STATUS_REQUESTED,
            LINK_STATUS_REJECTED,
        ]
        .contains(status_value)
        {
            assert_eq!(
                status, 200,
                "case {case:?}: expected this status to be rejectable, got {status} {body:?}"
            );
            assert_eq!(
                text(&body, "status"),
                LINK_STATUS_REJECTED,
                "case {case:?}: expected the declined link to be REJECTED, got {:?}",
                body["status"]
            );
        } else {
            assert_eq!(
                status, 409,
                "case {case:?}: expected this status to conflict with 409, got {status} {body:?}"
            );
            // The refused call left the link exactly as it was.
            let (_, read_back) = fabric.send("GET", &link, None).await;
            assert_eq!(
                text(&read_back, "status"),
                case,
                "case {case:?}: expected the refused reject to leave the status alone, got {:?}",
                read_back["status"]
            );
        }
    }
}

#[tokio::test]
async fn a_reject_of_a_link_the_gateway_is_not_an_end_of_is_not_found() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("token-a").await;
    // A second pair, so there is a gateway that is no end of the first link.
    let other = fabric.linked_pair("token-b").await;

    for (case, gateway_id, link_id) in [
        ("neither exists", "rtb-gw-unknown", "link-unknown"),
        (
            "a stored gateway, a link id nothing minted",
            pair.requester_id.as_str(),
            "link-unknown",
        ),
        (
            "a stored link, addressed from a gateway that is not one of its ends",
            other.responder_id.as_str(),
            pair.link_id.as_str(),
        ),
    ] {
        let (status, body) = fabric
            .send("POST", &format!("/gateway/{gateway_id}/link/{link_id}/reject"), None)
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected RejectLink to answer 404, got {status} {body:?}"
        );
    }

    // The link that was NOT addressable by those calls is untouched by them.
    let (status, read_back) = fabric
        .send(
            "GET",
            &format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "expected the untouched link to still be readable, got {status} {read_back:?}"
    );
    assert_ne!(
        text(&read_back, "status"),
        LINK_STATUS_REJECTED,
        "expected a refused reject to leave the link's status alone, got {:?}",
        read_back["status"]
    );
}

/// One accept row: whether the clock has settled `PENDING_REQUEST` ->
/// `REQUESTED` first, and which of `AcceptLink`'s optional members the call
/// states. Every row addresses the end the request was put to, the end the
/// operation's documentation describes as accepting it.
struct AcceptRow {
    name: &'static str,
    settle_first: bool,
    /// The `customerProvidedId` of the `attributes` the accept states, or `None`
    /// to leave the attributes the create stored in place.
    customer_provided_id: Option<&'static str>,
    /// A `timeoutInMillis`, or `None` to state none. The values stated here are
    /// the published bounds of the modelled range (100..5000).
    timeout_in_millis: Option<i64>,
    /// The `errorLog` sampling of the `logSettings` this call requires, at and
    /// between the published bounds of the modelled range (0..100).
    error_log: f64,
}

const ACCEPT_ROWS: &[AcceptRow] = &[
    AcceptRow {
        name: "the peer accepts an unsettled request with only the required settings",
        settle_first: false,
        customer_provided_id: None,
        timeout_in_millis: None,
        error_log: 0.0,
    },
    AcceptRow {
        name: "the peer accepts a settled request, stating every optional member",
        settle_first: true,
        customer_provided_id: Some("accepted-fabric"),
        timeout_in_millis: Some(100),
        error_log: 100.0,
    },
    AcceptRow {
        name: "the peer accepts a settled request, stating attributes only",
        settle_first: true,
        customer_provided_id: Some("fabric_2"),
        timeout_in_millis: None,
        error_log: 12.5,
    },
    AcceptRow {
        name: "the peer accepts a settled request, stating the timeout only",
        settle_first: true,
        customer_provided_id: None,
        timeout_in_millis: Some(5000),
        error_log: 50.0,
    },
];

/// The body one accept row sends: the required `logSettings`, plus the optional
/// members the row states.
fn accept_body(row: &AcceptRow) -> Value {
    let mut body = json!({
        "logSettings": {
            "applicationLogs": { "sampling": { "errorLog": row.error_log, "filterLog": 0.0 } }
        }
    });
    if let Some(customer_provided_id) = row.customer_provided_id {
        body["attributes"] = json!({ "customerProvidedId": customer_provided_id });
    }
    if let Some(timeout_in_millis) = row.timeout_in_millis {
        body["timeoutInMillis"] = json!(timeout_in_millis);
    }
    body
}

#[tokio::test]
async fn accepting_an_outstanding_request_drives_the_link_to_accepted_and_then_active() {
    for row in ACCEPT_ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        // The acceptance is the answer of the end the request was put to.
        let (addressed, other) = (&pair.responder_id, &pair.requester_id);
        if row.settle_first {
            // The move to REQUESTED is owed by the clock: poll for it.
            fabric
                .poll_link_status(addressed, &pair.link_id, LINK_STATUS_REQUESTED)
                .await;
        }

        let body = accept_body(row);
        let accept = format!("/gateway/{addressed}/link/{}/accept", pair.link_id);
        let (status, accepted) = fabric.send("POST", &accept, Some(body.clone())).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected AcceptLink to answer 200, got {status} {accepted:?}"
        );
        assert_eq!(
            text(&accepted, "status"),
            LINK_STATUS_ACCEPTED,
            "case {case:?}: expected the accepted link to be ACCEPTED, got {:?}",
            accepted["status"]
        );
        assert_eq!(
            text(&accepted, "linkId"),
            pair.link_id,
            "case {case:?}: expected link {:?}, got {:?}",
            pair.link_id,
            accepted["linkId"]
        );
        assert_eq!(
            text(&accepted, "gatewayId").as_str(),
            addressed.as_str(),
            "case {case:?}: expected the addressed gateway {addressed:?} as gatewayId, got {:?}",
            accepted["gatewayId"]
        );
        assert_eq!(
            text(&accepted, "peerGatewayId").as_str(),
            other.as_str(),
            "case {case:?}: expected the other end {other:?} as peerGatewayId, got {:?}",
            accepted["peerGatewayId"]
        );
        // The settings this call states are the link's from now on; the facts
        // the create left are unchanged by the accept.
        let expected_attributes = match row.customer_provided_id {
            Some(customer_provided_id) => json!({ "customerProvidedId": customer_provided_id }),
            // What `linked_pair`'s CreateLink stored, left in place by an accept
            // that states no attributes.
            None => json!({ "customerProvidedId": "spot-fabric" }),
        };
        let expected_timeout = match row.timeout_in_millis {
            Some(timeout_in_millis) => json!(timeout_in_millis),
            None => Value::Null,
        };
        for (member, expected) in [
            ("createdAt", pair.created_at.clone()),
            ("direction", json!("REQUEST")),
            ("connectivityType", json!("DEFAULT")),
            ("logSettings", body["logSettings"].clone()),
            ("attributes", expected_attributes.clone()),
        ] {
            assert_eq!(
                accepted[member], expected,
                "case {case:?}: expected {member} {expected:?}, got {:?}",
                accepted[member]
            );
        }

        // A repeat of this `@idempotent` call asks for the state the link is
        // already in, so it answers the record rather than refusing it.
        let (status, repeat) = fabric.send("POST", &accept, Some(body.clone())).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a repeated AcceptLink to answer 200, got {status} {repeat:?}"
        );
        let repeated_status = text(&repeat, "status");
        assert!(
            [LINK_STATUS_ACCEPTED, LINK_STATUS_ACTIVE].contains(&repeated_status.as_str()),
            "case {case:?}: expected the repeat to answer an accepted link, got {repeated_status:?}"
        );

        // The accepted link comes up, and every reader reads the accepted
        // record back: both ends, through `GetLink`.
        for (end, gateway_id) in [("addressed end", addressed), ("other end", other)] {
            let read_back = fabric
                .poll_link_status(gateway_id, &pair.link_id, LINK_STATUS_ACTIVE)
                .await;
            for (member, expected) in [
                ("createdAt", pair.created_at.clone()),
                ("logSettings", body["logSettings"].clone()),
                ("attributes", expected_attributes.clone()),
                ("timeoutInMillis", expected_timeout.clone()),
            ] {
                assert_eq!(
                    read_back[member], expected,
                    "case {case:?}: expected the {end} to read {member} {expected:?} back, got {:?}",
                    read_back[member]
                );
            }
        }
        // ACTIVE is where the accept leaves the link: nothing further is owed,
        // so no later reader sees it move on to FAILED or anything else.
        assert!(
            !fabric.settlement_owed(),
            "case {case:?}: expected nothing further to be owed on an ACTIVE link"
        );
    }
}

#[tokio::test]
async fn only_an_outstanding_or_an_already_accepted_request_can_be_accepted() {
    // The whole modelled catalog, from `LINK_STATUS_VALUES` (this service's
    // `LinkStatus` enum), so a status no operation reaches yet is covered too.
    for status_value in LINK_STATUS_VALUES {
        let case = *status_value;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        fabric.force_link_status(status_value);
        let link = format!("/gateway/{}/link/{}", pair.responder_id, pair.link_id);

        let (status, code, body) = fabric
            .send_with_code(
                "POST",
                &format!("{link}/accept"),
                Some(accept_body(&ACCEPT_ROWS[0])),
            )
            .await;

        if [LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REQUESTED].contains(status_value) {
            assert_eq!(
                status, 200,
                "case {case:?}: expected this status to be acceptable, got {status} {body:?}"
            );
            assert_eq!(
                text(&body, "status"),
                LINK_STATUS_ACCEPTED,
                "case {case:?}: expected the accepted link to be ACCEPTED, got {:?}",
                body["status"]
            );
        } else if [LINK_STATUS_ACCEPTED, LINK_STATUS_ACTIVE].contains(status_value) {
            // The accept this operation already recorded: a repeat answers the
            // record it left, so the status it reports is the stored one.
            assert_eq!(
                status, 200,
                "case {case:?}: expected an already accepted link to answer 200, got {status} {body:?}"
            );
            assert_eq!(
                text(&body, "status"),
                case,
                "case {case:?}: expected the stored status, got {:?}",
                body["status"]
            );
        } else {
            assert_eq!(
                status, 409,
                "case {case:?}: expected this status to conflict with 409, got {status} {body:?}"
            );
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected ConflictException, got {code:?}"
            );
            // The refusal names the link and the gateway the call addressed,
            // and the status that refuses it.
            let message = text(&body, "message");
            for value in [pair.link_id.as_str(), pair.responder_id.as_str(), case] {
                assert!(
                    message.contains(value),
                    "case {case:?}: expected the refusal to name {value:?}, got {message:?}"
                );
            }
            // The refused call left the link exactly as it was.
            let (_, read_back) = fabric.send("GET", &link, None).await;
            assert_eq!(
                text(&read_back, "status"),
                case,
                "case {case:?}: expected the refused accept to leave the status alone, got {:?}",
                read_back["status"]
            );
        }
    }
}

#[tokio::test]
async fn an_accept_of_a_link_the_gateway_is_not_an_end_of_is_not_found() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("token-a").await;
    // A second pair, so there is a gateway that is no end of the first link.
    let other = fabric.linked_pair("token-b").await;

    for (case, gateway_id, link_id) in [
        ("neither exists", "rtb-gw-unknown", "link-unknown"),
        (
            "a stored gateway, a link id nothing minted",
            pair.requester_id.as_str(),
            "link-unknown",
        ),
        (
            "a stored link, addressed from a gateway that is not one of its ends",
            other.responder_id.as_str(),
            pair.link_id.as_str(),
        ),
    ] {
        let (status, code, body) = fabric
            .send_with_code(
                "POST",
                &format!("/gateway/{gateway_id}/link/{link_id}/accept"),
                Some(accept_body(&ACCEPT_ROWS[0])),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected AcceptLink to answer 404, got {status} {body:?}"
        );
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?}"
        );
    }

    // The link that was NOT addressable by those calls is untouched by them.
    let (status, read_back) = fabric
        .send(
            "GET",
            &format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "expected the untouched link to still be readable, got {status} {read_back:?}"
    );
    let untouched = text(&read_back, "status");
    assert!(
        [LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REQUESTED].contains(&untouched.as_str()),
        "expected a refused accept to leave the link's request outstanding, got {untouched:?}"
    );
}

#[tokio::test]
async fn only_a_live_link_occupies_the_gateway_pair_it_joins() {
    // The whole modelled catalog again, from `LINK_STATUS_VALUES`, crossed with
    // the orientation of the fresh request: a link joins two gateways in both
    // directions, so which end opens the new one cannot change the answer.
    for status_value in LINK_STATUS_VALUES {
        for reversed in [false, true] {
            let case = format!(
                "{status_value}, opened from the {}",
                if reversed { "responder" } else { "requester" }
            );
            let fabric = Fabric::new();
            let pair = fabric.linked_pair("token").await;
            fabric.force_link_status(status_value);
            let (opener, peer) = if reversed {
                (&pair.responder_id, &pair.requester_id)
            } else {
                (&pair.requester_id, &pair.responder_id)
            };

            let (status, code, body) = fabric
                .send_with_code(
                    "POST",
                    &format!("/gateway/{opener}/create-link"),
                    Some(json!({
                        "peerGatewayId": peer,
                        "logSettings": log_settings(),
                    })),
                )
                .await;

            if [
                LINK_STATUS_REJECTED,
                LINK_STATUS_FAILED,
                LINK_STATUS_DELETED,
            ]
            .contains(status_value)
            {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected a terminal link to leave the pair free, got {status} {body:?}"
                );
                let fresh = text(&body, "linkId");
                assert_ne!(
                    fresh, pair.link_id,
                    "case {case:?}: expected a link of its own, got the terminal link's id"
                );
                // The fresh link is a record of its own: the terminal one is
                // still there, in the status it ended in.
                let (status, ended) = fabric
                    .send(
                        "GET",
                        &format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id),
                        None,
                    )
                    .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the terminal link to still be readable, got {status} {ended:?}"
                );
                assert_eq!(
                    text(&ended, "status"),
                    *status_value,
                    "case {case:?}: expected the terminal link to keep its status, got {:?}",
                    ended["status"]
                );
            } else {
                assert_eq!(
                    status, 409,
                    "case {case:?}: expected a live link to occupy the pair, got {status} {body:?}"
                );
                assert_eq!(
                    code, "ConflictException",
                    "case {case:?}: expected ConflictException, got {code:?}"
                );
            }
        }
    }
}

/// The configuration every update row starts from, stated on the accept that
/// brings the link up. None of its values is one an update row states, so a row
/// that changes a member and a row that leaves it alone cannot be confused.
const UPDATE_BASELINE: AcceptRow = AcceptRow {
    name: "the configuration an update row starts from",
    settle_first: false,
    customer_provided_id: Some("baseline-fabric"),
    timeout_in_millis: Some(1000),
    error_log: 75.0,
};

/// The `filterLog` sampling every stated `logSettings` carries: `logSettings` is
/// one structure, so stating it replaces the whole of what the record holds,
/// including the member the update does not care about.
const UPDATE_FILTER_LOG: f64 = 25.0;

/// One update row: which of `UpdateLink`'s two optional members the call states,
/// and which end of the link addresses it.
struct UpdateRow {
    name: &'static str,
    /// The `errorLog` sampling of the `logSettings` the update states, or `None`
    /// to state no `logSettings` at all. The values are at and between the
    /// published bounds of the modelled range (0..100).
    error_log: Option<f64>,
    /// A `timeoutInMillis`, or `None` to state none. The values are the
    /// published bounds of the modelled range (100..5000) and one between them,
    /// from `smithy_query(query="operation", operation="UpdateLink")`.
    timeout_in_millis: Option<i64>,
    /// `true` to address the update to the end the link request was put to,
    /// `false` to address it to the end that opened the link.
    from_peer: bool,
}

const UPDATE_ROWS: &[UpdateRow] = &[
    UpdateRow {
        name: "both members, the timeout at its lower bound",
        error_log: Some(100.0),
        timeout_in_millis: Some(100),
        from_peer: true,
    },
    UpdateRow {
        name: "both members, the timeout at its upper bound",
        error_log: Some(0.0),
        timeout_in_millis: Some(5000),
        from_peer: false,
    },
    UpdateRow {
        name: "the log settings only",
        error_log: Some(12.5),
        timeout_in_millis: None,
        from_peer: false,
    },
    UpdateRow {
        name: "the timeout only, between its bounds",
        error_log: None,
        timeout_in_millis: Some(2500),
        from_peer: true,
    },
    UpdateRow {
        name: "neither member",
        error_log: None,
        timeout_in_millis: None,
        from_peer: true,
    },
];

/// The body one update row sends: both of `UpdateLink`'s members are optional,
/// so a row states only the ones it changes.
fn update_body(row: &UpdateRow) -> Value {
    let mut body = json!({});
    if let Some(error_log) = row.error_log {
        body["logSettings"] = json!({
            "applicationLogs": {
                "sampling": { "errorLog": error_log, "filterLog": UPDATE_FILTER_LOG }
            }
        });
    }
    if let Some(timeout_in_millis) = row.timeout_in_millis {
        body["timeoutInMillis"] = json!(timeout_in_millis);
    }
    body
}

impl UpdateRow {
    /// Whether this row asks for a change at all: both configuration members are
    /// optional, so a row that states neither asks for nothing.
    fn states_a_change(&self) -> bool {
        self.error_log.is_some() || self.timeout_in_millis.is_some()
    }
}

/// One sampling member of a link reading's `logSettings`.
fn sampling(body: &Value, member: &str) -> f64 {
    body["logSettings"]["applicationLogs"]["sampling"][member]
        .as_f64()
        .unwrap_or(f64::NAN)
}

#[tokio::test]
async fn an_update_replaces_the_members_it_states_and_keeps_the_rest() {
    for row in UPDATE_ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        // Bring the link up first: an update is a change to a configuration the
        // link already carries.
        let (status, accepted) = fabric
            .send(
                "POST",
                &format!("/gateway/{}/link/{}/accept", pair.responder_id, pair.link_id),
                Some(accept_body(&UPDATE_BASELINE)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the accept to answer 200, got {status} {accepted:?}"
        );
        let before = fabric
            .poll_link_status(&pair.requester_id, &pair.link_id, LINK_STATUS_ACTIVE)
            .await;

        let (addressed, other) = if row.from_peer {
            (&pair.responder_id, &pair.requester_id)
        } else {
            (&pair.requester_id, &pair.responder_id)
        };
        let (status, updated) = fabric
            .send(
                "PATCH",
                &format!("/gateway/{addressed}/link/{}", pair.link_id),
                Some(update_body(row)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateLink to answer 200, got {status} {updated:?}"
        );
        assert_eq!(
            text(&updated, "linkId"),
            pair.link_id,
            "case {case:?}: expected link {:?}, got {:?}",
            pair.link_id,
            updated["linkId"]
        );
        // A stated change is applied to a link that carries traffic, which is
        // what PENDING_UPDATE reports; a call that states neither member asks
        // for no change, so the link stays in the status it already stood in.
        let expected_status = if row.states_a_change() {
            LINK_STATUS_PENDING_UPDATE
        } else {
            LINK_STATUS_ACTIVE
        };
        assert_eq!(
            text(&updated, "status"),
            expected_status,
            "case {case:?}: expected the update to report {expected_status:?}, got {:?}",
            updated["status"]
        );
        // The clock owes the link its way back up only for a call that carried a
        // change: one that carried none left nothing in flight.
        assert_eq!(
            fabric.settlement_owed(),
            row.states_a_change(),
            "case {case:?}: expected a transition to be owed only for a stated change, got {}",
            fabric.settlement_owed()
        );

        // What the update left, read back from BOTH ends once the clock has
        // brought the link back up.
        let expected_error_log = row.error_log.unwrap_or_else(|| sampling(&before, "errorLog"));
        let expected_filter_log = if row.error_log.is_some() {
            UPDATE_FILTER_LOG
        } else {
            sampling(&before, "filterLog")
        };
        let expected_timeout = row
            .timeout_in_millis
            .map_or_else(|| before["timeoutInMillis"].clone(), |value| json!(value));
        for end in [addressed, other] {
            let read_back = fabric
                .poll_link_status(end, &pair.link_id, LINK_STATUS_ACTIVE)
                .await;
            assert_eq!(
                sampling(&read_back, "errorLog"),
                expected_error_log,
                "case {case:?}: expected errorLog {expected_error_log} from {end}, got {:?}",
                read_back["logSettings"]
            );
            assert_eq!(
                sampling(&read_back, "filterLog"),
                expected_filter_log,
                "case {case:?}: expected filterLog {expected_filter_log} from {end}, got {:?}",
                read_back["logSettings"]
            );
            assert_eq!(
                read_back["timeoutInMillis"], expected_timeout,
                "case {case:?}: expected timeoutInMillis {expected_timeout:?} from {end}, got {:?}",
                read_back["timeoutInMillis"]
            );
            // Everything the update does not state is the record's, unchanged.
            for member in ["attributes", "direction", "connectivityType", "createdAt"] {
                assert_eq!(
                    read_back[member], before[member],
                    "case {case:?}: expected {member} to be left alone, was {:?}, got {:?}",
                    before[member], read_back[member]
                );
            }
            if row.states_a_change() {
                // The record did change, so it is dated at the instant it changed.
                assert_ne!(
                    read_back["updatedAt"], before["updatedAt"],
                    "case {case:?}: expected the update to re-date the link, got {:?}",
                    read_back["updatedAt"]
                );
            } else {
                // Nothing was asked for, so nothing about the record moved: the
                // link is still dated at the instant it last changed.
                assert_eq!(
                    read_back["updatedAt"], before["updatedAt"],
                    "case {case:?}: expected a call stating no member to leave updatedAt at {:?}, got {:?}",
                    before["updatedAt"], read_back["updatedAt"]
                );
            }
        }
        // ACTIVE is where the update leaves the link: nothing further is owed.
        assert!(
            !fabric.settlement_owed(),
            "case {case:?}: expected nothing further to be owed on an updated link"
        );
    }
}

#[tokio::test]
async fn only_a_link_in_a_status_it_can_leave_can_be_configured() {
    // The whole modelled catalog, from `LINK_STATUS_VALUES` (this service's
    // `LinkStatus` enum), so a status no operation reaches yet is covered too.
    for status_value in LINK_STATUS_VALUES {
        let case = *status_value;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        fabric.force_link_status(status_value);
        let link = format!("/gateway/{}/link/{}", pair.responder_id, pair.link_id);
        let (_, before) = fabric.send("GET", &link, None).await;

        let (status, code, body) = fabric
            .send_with_code("PATCH", &link, Some(json!({ "timeoutInMillis": 2500 })))
            .await;

        if [
            LINK_STATUS_REJECTED,
            LINK_STATUS_FAILED,
            LINK_STATUS_DELETED,
        ]
        .contains(status_value)
        {
            assert_eq!(
                status, 409,
                "case {case:?}: expected a status nothing leaves to conflict with 409, got {status} {body:?}"
            );
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected ConflictException, got {code:?}"
            );
            // The refusal names the link and the gateway the call addressed,
            // and the status that refuses it.
            let message = text(&body, "message");
            for value in [pair.link_id.as_str(), pair.responder_id.as_str(), case] {
                assert!(
                    message.contains(value),
                    "case {case:?}: expected the refusal to name {value:?}, got {message:?}"
                );
            }
            let (_, read_back) = fabric.send("GET", &link, None).await;
            assert_eq!(
                read_back["timeoutInMillis"], before["timeoutInMillis"],
                "case {case:?}: expected the refused update to store nothing, got {:?}",
                read_back["timeoutInMillis"]
            );
            assert_eq!(
                text(&read_back, "status"),
                case,
                "case {case:?}: expected the refused update to leave the status alone, got {:?}",
                read_back["status"]
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected UpdateLink to answer 200, got {status} {body:?}"
            );
            // A link that is up takes the change while it carries traffic; one
            // still moving through its own lifecycle keeps the status that
            // lifecycle owes it.
            let expected = if [LINK_STATUS_ACTIVE, LINK_STATUS_PENDING_UPDATE].contains(status_value)
            {
                LINK_STATUS_PENDING_UPDATE
            } else {
                case
            };
            assert_eq!(
                text(&body, "status"),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                body["status"]
            );
            assert_eq!(
                text(&body, "linkId"),
                pair.link_id,
                "case {case:?}: expected link {:?}, got {:?}",
                pair.link_id,
                body["linkId"]
            );
            // Whatever the status, the configuration the update stated is what
            // the next reader reports.
            let (_, read_back) = fabric.send("GET", &link, None).await;
            assert_eq!(
                read_back["timeoutInMillis"],
                json!(2500),
                "case {case:?}: expected the stored timeout to be read back, got {:?}",
                read_back["timeoutInMillis"]
            );
        }
    }
}

#[tokio::test]
async fn an_update_of_a_link_the_gateway_is_not_an_end_of_is_not_found() {
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("token-a").await;
    // A second pair, so there is a gateway that is no end of the first link.
    let other = fabric.linked_pair("token-b").await;

    for (case, gateway_id, link_id) in [
        ("neither exists", "rtb-gw-unknown", "link-unknown"),
        (
            "a stored gateway, a link id nothing minted",
            pair.requester_id.as_str(),
            "link-unknown",
        ),
        (
            "a stored link, addressed from a gateway that is not one of its ends",
            other.responder_id.as_str(),
            pair.link_id.as_str(),
        ),
    ] {
        let (status, code, body) = fabric
            .send_with_code(
                "PATCH",
                &format!("/gateway/{gateway_id}/link/{link_id}"),
                Some(json!({ "timeoutInMillis": 2500 })),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected UpdateLink to answer 404, got {status} {body:?}"
        );
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?}"
        );
    }

    // The link those calls could not address is untouched by them.
    let (status, read_back) = fabric
        .send(
            "GET",
            &format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "expected the untouched link to still be readable, got {status} {read_back:?}"
    );
    assert_eq!(
        read_back["timeoutInMillis"],
        Value::Null,
        "expected a refused update to store nothing, got {:?}",
        read_back["timeoutInMillis"]
    );
}

#[tokio::test]
async fn an_update_states_a_timeout_inside_the_modelled_range() {
    // The published bounds of `LinkTimeoutInMillis` (range 100..5000), from
    // `smithy_query(query="operation", operation="UpdateLink")`, and one past
    // each of them.
    let fabric = Fabric::new();
    let pair = fabric.linked_pair("token").await;
    let link = format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id);

    for (timeout_in_millis, expected) in [(99_i64, 400_u16), (100, 200), (5000, 200), (5001, 400)] {
        let case = timeout_in_millis;
        let (status, code, body) = fabric
            .send_with_code(
                "PATCH",
                &link,
                Some(json!({ "timeoutInMillis": timeout_in_millis })),
            )
            .await;
        assert_eq!(
            status, expected,
            "case {case}: expected {expected}, got {status} {body:?}"
        );
        if expected == 400 {
            assert_eq!(
                code, "ValidationException",
                "case {case}: expected ValidationException, got {code:?}"
            );
        }
    }

    // The last value inside the range is what the link reports: neither refusal
    // stored anything.
    let (_, read_back) = fabric.send("GET", &link, None).await;
    assert_eq!(
        read_back["timeoutInMillis"],
        json!(5000),
        "expected the last accepted timeout to be read back, got {:?}",
        read_back["timeoutInMillis"]
    );
}

/// The statuses a link can still be configured from: every member of the
/// modelled `LinkStatus` enum except the three nothing moves a link out of.
fn configurable_link_statuses() -> Vec<&'static str> {
    LINK_STATUS_VALUES
        .iter()
        .copied()
        .filter(|status| {
            ![
                LINK_STATUS_REJECTED,
                LINK_STATUS_FAILED,
                LINK_STATUS_DELETED,
            ]
            .contains(status)
        })
        .collect()
}

/// The forms a request that states neither configuration member can take on the
/// wire: both of `UpdateLink`'s body members are optional, so an empty object and
/// no body at all state exactly the same thing — nothing.
fn bodies_stating_no_member() -> [(&'static str, Option<Value>); 2] {
    [
        ("an empty object", Some(json!({}))),
        ("no body at all", None),
    ]
}

#[tokio::test]
async fn an_update_that_states_no_configuration_member_leaves_the_link_alone() {
    // Spans what the rule generalises over: the status the link stands in (the
    // whole modelled catalog, from `LINK_STATUS_VALUES`), which of the link's
    // two ends addresses the call, and the form a request stating nothing takes.
    for status_value in configurable_link_statuses() {
        for from_peer in [false, true] {
            for (form, body) in bodies_stating_no_member() {
                let case = format!("{status_value} addressed from the {} end, {form}", {
                    if from_peer {
                        "answering"
                    } else {
                        "opening"
                    }
                });
                let case = case.as_str();
                let fabric = Fabric::new();
                let pair = fabric.linked_pair("token").await;
                fabric.force_link_status(status_value);
                let addressed = if from_peer {
                    &pair.responder_id
                } else {
                    &pair.requester_id
                };
                let link = format!("/gateway/{addressed}/link/{}", pair.link_id);
                let (_, before) = fabric.send("GET", &link, None).await;

                let (status, updated) = fabric.send("PATCH", &link, body).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected a request stating no member to be served, got {status} {updated:?}"
                );
                assert_eq!(
                    text(&updated, "linkId"),
                    pair.link_id,
                    "case {case:?}: expected link {:?}, got {:?}",
                    pair.link_id,
                    updated["linkId"]
                );
                // Nothing was asked for, so the link is reported in the status
                // it already stood in rather than as taking a change.
                assert_eq!(
                    text(&updated, "status"),
                    status_value,
                    "case {case:?}: expected the link's own status {status_value:?}, got {:?}",
                    updated["status"]
                );
                // And nothing is in flight: no transition was started, so the
                // clock owes the link none.
                assert!(
                    !fabric.settlement_owed(),
                    "case {case:?}: expected no transition to be owed after a call that stated nothing"
                );

                // The whole record is what it was: the configuration, the status
                // and the `updatedAt` every later reader reports.
                let (status, after) = fabric.send("GET", &link, None).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the link to still read back, got {status} {after:?}"
                );
                assert_eq!(
                    after, before,
                    "case {case:?}: expected the record to be untouched, got {after:?} after {before:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_link_in_a_terminal_status_is_refused_even_by_a_request_that_states_nothing() {
    // The refusal is about the link's status, not about what the call states, so
    // it holds for a request that asks for no change at all.
    for status_value in [
        LINK_STATUS_REJECTED,
        LINK_STATUS_FAILED,
        LINK_STATUS_DELETED,
    ] {
        for (form, body) in bodies_stating_no_member() {
            let case = format!("{status_value}, {form}");
            let case = case.as_str();
            let fabric = Fabric::new();
            let pair = fabric.linked_pair("token").await;
            fabric.force_link_status(status_value);
            let link = format!("/gateway/{}/link/{}", pair.requester_id, pair.link_id);
            let (_, before) = fabric.send("GET", &link, None).await;

            let (status, code, refused) = fabric.send_with_code("PATCH", &link, body).await;
            assert_eq!(
                status, 409,
                "case {case:?}: expected a status nothing leaves to conflict with 409, got {status} {refused:?}"
            );
            assert_eq!(
                code, "ConflictException",
                "case {case:?}: expected ConflictException, got {code:?}"
            );
            assert_eq!(
                text(&refused, "message"),
                format!(
                    "link {} on gateway {} is {status_value} and cannot be updated",
                    pair.link_id, pair.requester_id
                ),
                "case {case:?}: expected the refusal to name the link, the gateway and the status, got {refused:?}"
            );
            let (_, after) = fabric.send("GET", &link, None).await;
            assert_eq!(
                after, before,
                "case {case:?}: expected the refused call to change nothing, got {after:?} after {before:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The counters the two gateway readers DERIVE from the statuses this suite
// drives: `GetRequesterGateway`'s `activeLinksCount`/`totalLinksCount` and
// `GetResponderGateway`'s those two plus `linksRequestedCount`.
//
// Two rules, both about the stored links rather than about one request:
//
// * a link counts as active for each of its ends exactly while it is carrying
//   traffic, which this service reports as `ACTIVE` and as the `PENDING_UPDATE`
//   of a live link that has taken a configuration change — the status the
//   update above leaves — so a link under an update is still an active link;
// * a link counts as requested for a gateway only while its request is still
//   outstanding AND that gateway is the end that must answer it, which is the
//   end the link was NOT opened from. `linksRequestedCount` is modelled as "The
//   count of requested links waiting for the responder gateway to accept or
//   reject" (`smithy_query(query="doc", shape="GetResponderGatewayResponse",
//   member="linksRequestedCount")`), and a responder gateway that opened a link
//   itself is waiting for nothing.
// ---------------------------------------------------------------------------

/// The statuses in which a link is carrying traffic, which is what the active
/// counter counts: the live link, and the live link to which a configuration
/// change it has already taken is being applied.
const CARRYING_TRAFFIC: &[&str] = &[LINK_STATUS_ACTIVE, LINK_STATUS_PENDING_UPDATE];

/// The statuses in which a link request is still outstanding, so the end it was
/// put to can still accept or decline it.
const REQUEST_OUTSTANDING: &[&str] = &[LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REQUESTED];

/// One counter row: which kind of gateway opened the link, which is what decides
/// which of its two ends has to answer the request.
struct CounterRow {
    name: &'static str,
    opened_by_requester: bool,
    /// The `LinkDirection` the service reads from the opener's kind.
    direction: &'static str,
}

const COUNTER_ROWS: &[CounterRow] = &[
    CounterRow {
        name: "the requester gateway opened the link",
        opened_by_requester: true,
        direction: "REQUEST",
    },
    CounterRow {
        name: "the responder gateway opened the link",
        opened_by_requester: false,
        direction: "RESPONSE",
    },
];

#[tokio::test]
async fn the_derived_counters_follow_the_link_status_and_the_end_that_must_answer() {
    for row in COUNTER_ROWS {
        let fabric = Fabric::new();
        let (requester_id, responder_id) = fabric.gateway_pair("token").await;
        let (opener_id, peer_id) = match row.opened_by_requester {
            true => (&requester_id, &responder_id),
            false => (&responder_id, &requester_id),
        };
        let link = fabric.open_link(opener_id, peer_id).await;
        assert_eq!(
            text(&link, "direction"),
            row.direction,
            "case {:?}: expected the direction the opener's kind gives the link, got {:?}",
            row.name,
            link["direction"]
        );

        // The whole modelled catalog, from `LINK_STATUS_VALUES` (this service's
        // `LinkStatus` enum), so a status no operation reaches yet is covered too.
        for status_value in LINK_STATUS_VALUES {
            let case = format!("{}, the link is {status_value}", row.name);
            fabric.force_link_status(status_value);

            let expected_active = i64::from(CARRYING_TRAFFIC.contains(status_value));
            // An outstanding request is waiting for the end it was put to, which
            // is the end that did not open the link.
            let expected_requested =
                i64::from(REQUEST_OUTSTANDING.contains(status_value) && row.opened_by_requester);

            let responder = fabric
                .gateway(&case, &format!("/responder-gateway/{responder_id}"))
                .await;
            assert_eq!(
                responder["activeLinksCount"],
                json!(expected_active),
                "case {case:?}: expected the responder gateway's activeLinksCount {expected_active}, got {:?}",
                responder["activeLinksCount"]
            );
            assert_eq!(
                responder["linksRequestedCount"],
                json!(expected_requested),
                "case {case:?}: expected the responder gateway's linksRequestedCount {expected_requested}, got {:?}",
                responder["linksRequestedCount"]
            );
            assert_eq!(
                responder["totalLinksCount"],
                json!(1),
                "case {case:?}: expected the responder gateway's totalLinksCount 1, got {:?}",
                responder["totalLinksCount"]
            );

            let requester = fabric
                .gateway(&case, &format!("/requester-gateway/{requester_id}"))
                .await;
            assert_eq!(
                requester["activeLinksCount"],
                json!(expected_active),
                "case {case:?}: expected the requester gateway's activeLinksCount {expected_active}, got {:?}",
                requester["activeLinksCount"]
            );
            assert_eq!(
                requester["totalLinksCount"],
                json!(1),
                "case {case:?}: expected the requester gateway's totalLinksCount 1, got {:?}",
                requester["totalLinksCount"]
            );
            // `GetRequesterGatewayResponse` models no `linksRequestedCount`: the
            // inbound queue belongs to the end that answers link requests.
            assert!(
                requester["linksRequestedCount"].is_null(),
                "case {case:?}: expected no linksRequestedCount on the requester gateway, got {:?}",
                requester["linksRequestedCount"]
            );
        }
    }
}

#[tokio::test]
async fn a_live_link_applying_a_configuration_change_still_counts_as_active() {
    // The update table above, minus the row that states no member at all: that
    // row asks for no change, so it leaves the link ACTIVE rather than applying
    // anything. Every other combination of the members the call may state, and
    // either end addressing it, reaches PENDING_UPDATE here.
    for row in UPDATE_ROWS.iter().filter(|row| row.states_a_change()) {
        let case = row.name;
        let fabric = Fabric::new();
        let pair = fabric.linked_pair("token").await;
        let (status, accepted) = fabric
            .send(
                "POST",
                &format!("/gateway/{}/link/{}/accept", pair.responder_id, pair.link_id),
                Some(accept_body(&UPDATE_BASELINE)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the accept to answer 200, got {status} {accepted:?}"
        );
        fabric
            .poll_link_status(&pair.requester_id, &pair.link_id, LINK_STATUS_ACTIVE)
            .await;

        let addressed = if row.from_peer {
            &pair.responder_id
        } else {
            &pair.requester_id
        };
        let (status, updated) = fabric
            .send(
                "PATCH",
                &format!("/gateway/{addressed}/link/{}", pair.link_id),
                Some(update_body(row)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected UpdateLink to answer 200, got {status} {updated:?}"
        );
        assert_eq!(
            text(&updated, "status"),
            LINK_STATUS_PENDING_UPDATE,
            "case {case:?}: expected the update of a live link to report it pending, got {:?}",
            updated["status"]
        );

        // The link carries traffic throughout — before the change, while it is
        // being applied, and after the clock has settled it — so both ends count
        // it as one of their active links whenever these reads land, and neither
        // end is waiting for a request to be answered.
        for (kind, path) in [
            ("requester", format!("/requester-gateway/{}", pair.requester_id)),
            ("responder", format!("/responder-gateway/{}", pair.responder_id)),
        ] {
            let gateway = fabric.gateway(case, &path).await;
            assert_eq!(
                gateway["activeLinksCount"],
                json!(1),
                "case {case:?}: expected the {kind} gateway to count the updating link as active, got {:?}",
                gateway["activeLinksCount"]
            );
            assert_eq!(
                gateway["totalLinksCount"],
                json!(1),
                "case {case:?}: expected the {kind} gateway's totalLinksCount 1, got {:?}",
                gateway["totalLinksCount"]
            );
        }
        let responder = fabric
            .gateway(case, &format!("/responder-gateway/{}", pair.responder_id))
            .await;
        assert_eq!(
            responder["linksRequestedCount"],
            json!(0),
            "case {case:?}: expected no outstanding request once the link is up, got {:?}",
            responder["linksRequestedCount"]
        );
    }
}

#[tokio::test]
async fn a_responder_gateway_counts_only_the_link_requests_put_to_it() {
    let case = "one request put to the responder gateway, one the responder gateway opened";
    let fabric = Fabric::new();
    // Two pairs, for the three gateways this needs: the responder gateway under
    // test, the requester gateway that asks it for a link, and a second
    // requester gateway for the responder to ask in turn.
    let (asking_id, responder_id) = fabric.gateway_pair("token-a").await;
    let (asked_id, _) = fabric.gateway_pair("token-b").await;

    let inbound = fabric.open_link(&asking_id, &responder_id).await;
    let inbound_id = text(&inbound, "linkId");
    let self_opened = fabric.open_link(&responder_id, &asked_id).await;
    let self_opened_id = text(&self_opened, "linkId");

    // Both requests are outstanding; poll them to the status the clock owes so
    // the reading below is taken with nothing owed on either.
    fabric
        .poll_link_status(&asking_id, &inbound_id, LINK_STATUS_REQUESTED)
        .await;
    fabric
        .poll_link_status(&responder_id, &self_opened_id, LINK_STATUS_REQUESTED)
        .await;

    let responder = fabric
        .gateway(case, &format!("/responder-gateway/{responder_id}"))
        .await;
    assert_eq!(
        responder["totalLinksCount"],
        json!(2),
        "case {case:?}: expected both links to count against the responder gateway, got {:?}",
        responder["totalLinksCount"]
    );
    assert_eq!(
        responder["linksRequestedCount"],
        json!(1),
        "case {case:?}: expected only the request put TO the responder gateway to be counted, got {:?}",
        responder["linksRequestedCount"]
    );
    assert_eq!(
        responder["activeLinksCount"],
        json!(0),
        "case {case:?}: expected no active link while both requests are outstanding, got {:?}",
        responder["activeLinksCount"]
    );

    // Answering the inbound request empties the inbound queue: the link the
    // responder gateway opened itself was never in it.
    let (status, answered) = fabric
        .send(
            "POST",
            &format!("/gateway/{responder_id}/link/{inbound_id}/accept"),
            Some(json!({ "logSettings": log_settings() })),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AcceptLink to answer 200, got {status} {answered:?}"
    );
    fabric
        .poll_link_status(&responder_id, &inbound_id, LINK_STATUS_ACTIVE)
        .await;

    let responder = fabric
        .gateway(case, &format!("/responder-gateway/{responder_id}"))
        .await;
    assert_eq!(
        responder["linksRequestedCount"],
        json!(0),
        "case {case:?}: expected the answered request to leave the inbound queue empty, got {:?}",
        responder["linksRequestedCount"]
    );
    assert_eq!(
        responder["activeLinksCount"],
        json!(1),
        "case {case:?}: expected the accepted link to count as active, got {:?}",
        responder["activeLinksCount"]
    );

    // The gateway the responder's own request was put to counts that link,
    // because it is the end which has to answer it.
    let asked = fabric
        .gateway(case, &format!("/requester-gateway/{asked_id}"))
        .await;
    assert_eq!(
        asked["totalLinksCount"],
        json!(1),
        "case {case:?}: expected the peer of the self-opened link to count it, got {:?}",
        asked["totalLinksCount"]
    );
}
