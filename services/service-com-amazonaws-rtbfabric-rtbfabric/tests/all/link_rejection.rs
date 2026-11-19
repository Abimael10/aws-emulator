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
//! addresses the call, whether the request has settled yet, which of the
//! accept's optional members the call states, and every member of the modelled
//! `LinkStatus` enum.
//!
//! The decline's own consequence is covered here too: a link that has reached a
//! terminal status occupies the gateway pair no longer, so the two gateways can
//! be linked again.
//!
//! `UpdateLink` is covered here as well, because it is what happens next to a
//! link an accept has brought up: it replaces the configuration members the
//! call states, keeps every member it leaves out, and reports the status that
//! change leaves the link in. Its table spans which of the two optional members
//! the call states, the published bounds of the modelled timeout range, which
//! end addresses the call, and every member of the modelled `LinkStatus` enum.

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

    /// A requester gateway, a responder gateway and a link opened from the
    /// requester to the responder, all over the wire.
    async fn linked_pair(&self, token: &str) -> Pair {
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
        let requester_id = text(&requester, "gatewayId");
        let responder_id = text(&responder, "gatewayId");
        let (status, link) = self
            .send(
                "POST",
                &format!("/gateway/{requester_id}/create-link"),
                Some(json!({
                    "peerGatewayId": responder_id,
                    "logSettings": log_settings(),
                    "attributes": { "customerProvidedId": "spot-fabric" },
                })),
            )
            .await;
        assert_eq!(status, 200, "CreateLink: {status} {link:?}");
        Pair {
            requester_id,
            responder_id,
            link_id: text(&link, "linkId"),
            created_at: link["createdAt"].clone(),
        }
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

/// One accept row: which end answers the request, whether the clock has settled
/// `PENDING_REQUEST` -> `REQUESTED` first, and which of `AcceptLink`'s optional
/// members the call states.
struct AcceptRow {
    name: &'static str,
    /// `true` for the gateway the request was put to (the end the operation's
    /// documentation describes as accepting it), `false` for the opener.
    from_peer: bool,
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
        from_peer: true,
        settle_first: false,
        customer_provided_id: None,
        timeout_in_millis: None,
        error_log: 0.0,
    },
    AcceptRow {
        name: "the peer accepts a settled request, stating every optional member",
        from_peer: true,
        settle_first: true,
        customer_provided_id: Some("accepted-fabric"),
        timeout_in_millis: Some(100),
        error_log: 100.0,
    },
    AcceptRow {
        name: "the peer accepts a settled request, stating attributes only",
        from_peer: true,
        settle_first: true,
        customer_provided_id: Some("fabric_2"),
        timeout_in_millis: None,
        error_log: 12.5,
    },
    AcceptRow {
        name: "the opener accepts its own settled request, stating the timeout only",
        from_peer: false,
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
        let (addressed, other) = if row.from_peer {
            (&pair.responder_id, &pair.requester_id)
        } else {
            (&pair.requester_id, &pair.responder_id)
        };
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
    from_peer: true,
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
        // The link was carrying traffic, so the change is applied to a live
        // link: the update is reported as pending, and the clock owes the link
        // its way back up.
        assert_eq!(
            text(&updated, "status"),
            LINK_STATUS_PENDING_UPDATE,
            "case {case:?}: expected the update to report PENDING_UPDATE, got {:?}",
            updated["status"]
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
            // The record did change, so it is dated at the instant it changed.
            assert_ne!(
                read_back["updatedAt"], before["updatedAt"],
                "case {case:?}: expected the update to re-date the link, got {:?}",
                read_back["updatedAt"]
            );
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
