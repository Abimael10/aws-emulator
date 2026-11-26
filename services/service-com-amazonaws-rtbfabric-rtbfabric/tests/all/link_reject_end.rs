// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for WHICH end of a link may decline its request.
//!
//! A link is opened from one end and answered by the OTHER, and `RejectLink` is
//! one of the two answers: only the end the request was put to can decline it,
//! so a decline addressed to the end the link was opened FROM is refused with
//! `ConflictException` (409) and leaves the record exactly as it stands — the
//! request stays outstanding for both ends, and the gateway pair stays occupied
//! by it.
//!
//! The rule is about which end the call addressed, never about which kind of
//! gateway that end is or how far the link has got, so the tables span what it
//! generalises over: which kind of gateway opened the link (a requester, which
//! makes it a `REQUEST` link, and a responder, which makes it a `RESPONSE` one),
//! whether the clock has settled `PENDING_REQUEST` -> `REQUESTED` before the
//! call, and every member of the modelled `LinkStatus` enum.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::LINK_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    ComAmazonawsRtbfabricRtbfabricState, LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REJECTED,
    LINK_STATUS_REQUESTED,
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

    /// Send one restJson1 request and return `(status, the wire error code from
    /// `x-amzn-errortype`, parsed body)`: a refusal is a status AND a code, and a
    /// caller's SDK dispatches on the code.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, String, Value) {
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

    /// A requester gateway and a responder gateway, both over the wire, as
    /// `(the requester's id, the responder's id)`.
    async fn gateway_pair(&self, case: &str, token: &str) -> (String, String) {
        let (status, _, requester) = self
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
        let (status, _, responder) = self
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
    /// its link id. Either kind of gateway can be the opener.
    async fn open_link(&self, case: &str, opener_id: &str, peer_id: &str) -> String {
        let (status, _, link) = self
            .send(
                "POST",
                &format!("/gateway/{opener_id}/create-link"),
                Some(json!({
                    "peerGatewayId": peer_id,
                    "logSettings": log_settings(),
                    "attributes": { "customerProvidedId": "reject-end" },
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
        let (status, _, body) = self
            .send("GET", &format!("/gateway/{gateway_id}/link/{link_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetLink on gateway {gateway_id:?} to answer 200, got {status} {body:?}"
        );
        body
    }

    /// `RejectLink` on one link, addressed to `gateway_id`.
    async fn reject(&self, gateway_id: &str, link_id: &str) -> (u16, String, Value) {
        self.send(
            "POST",
            &format!("/gateway/{gateway_id}/link/{link_id}/reject"),
            None,
        )
        .await
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

    /// Put `status` on every stored link.
    ///
    /// The statuses a link reaches beyond its own request and acceptance are
    /// driven by operations this crate has not implemented yet, so the state is
    /// set directly to cover the whole modelled enum rather than only the part
    /// the wire reaches today.
    fn force_link_status(&self, status: &str) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let mut store = state.lock();
        for link in store.links.values_mut() {
            link.status = status.to_string();
            link.settles_at = None;
        }
    }
}

/// Assert a decline addressed to the end the link was opened from was refused as
/// the rule says, and answer with nothing: the caller checks the record itself.
fn assert_refused(case: &str, status: u16, code: &str, body: &Value, named: [&str; 3]) {
    assert_eq!(
        status, 409,
        "case {case:?}: expected the opening end's reject to answer 409, got {status} {body:?}"
    );
    assert_eq!(
        code, "ConflictException",
        "case {case:?}: expected ConflictException, got {code:?}"
    );
    let message = text(body, "message");
    for value in named {
        assert!(
            message.contains(value),
            "case {case:?}: expected the refusal to name {value:?}, got {message:?}"
        );
    }
}

/// One row: which kind of gateway opened the link, and whether the clock has
/// settled the request before the decline is addressed to the opening end.
struct Row {
    name: &'static str,
    /// `true` when the requester gateway opened the link (a `REQUEST` link),
    /// `false` when the responder gateway opened it (a `RESPONSE` one).
    opened_by_requester: bool,
    settle_first: bool,
}

const ROWS: &[Row] = &[
    Row {
        name: "a requester-opened request, unsettled",
        opened_by_requester: true,
        settle_first: false,
    },
    Row {
        name: "a requester-opened request, settled",
        opened_by_requester: true,
        settle_first: true,
    },
    Row {
        name: "a responder-opened request, unsettled",
        opened_by_requester: false,
        settle_first: false,
    },
    Row {
        name: "a responder-opened request, settled",
        opened_by_requester: false,
        settle_first: true,
    },
];

#[tokio::test]
async fn only_the_end_a_link_request_was_put_to_can_decline_it() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let (requester_id, responder_id) = fabric.gateway_pair(case, "reject-end").await;
        let (opener_id, peer_id) = if row.opened_by_requester {
            (requester_id.clone(), responder_id.clone())
        } else {
            (responder_id.clone(), requester_id.clone())
        };
        let link_id = fabric.open_link(case, &opener_id, &peer_id).await;

        let outstanding = if row.settle_first {
            // The move to REQUESTED is owed by the clock: poll for it rather
            // than wait a span.
            fabric
                .poll_link_status(case, &opener_id, &link_id, LINK_STATUS_REQUESTED)
                .await;
            LINK_STATUS_REQUESTED
        } else {
            LINK_STATUS_PENDING_REQUEST
        };

        // The end that ASKED for the link has no decline to give.
        let (status, code, refused) = fabric.reject(&opener_id, &link_id).await;
        assert_refused(
            case,
            status,
            code.as_str(),
            &refused,
            [link_id.as_str(), opener_id.as_str(), peer_id.as_str()],
        );

        // The refused call left the request outstanding for both readers: no end
        // was told the peer had declined anything. The request the row started
        // from is outstanding, and the clock may since have put it to the peer,
        // so either outstanding status is what a reader may see here.
        for (end, gateway_id) in [("opening end", &opener_id), ("peer end", &peer_id)] {
            let reading = fabric.read_link(case, gateway_id, &link_id).await;
            let seen = text(&reading, "status");
            assert!(
                [LINK_STATUS_PENDING_REQUEST, LINK_STATUS_REQUESTED].contains(&seen.as_str()),
                "case {case:?}: expected the {end} to still read the request it left outstanding ({outstanding:?} or later), got {seen:?}"
            );
        }

        // And the request still occupies the gateway pair, so the opening end
        // cannot clear it out of the peer's way with a fresh request either.
        let (status, code, conflicting) = fabric
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
            status, 409,
            "case {case:?}: expected the pair to be occupied by the outstanding request, got {status} {conflicting:?}"
        );
        assert_eq!(
            code, "ConflictException",
            "case {case:?}: expected ConflictException for the occupied pair, got {code:?}"
        );

        // The end the request WAS put to declines it, and that is what ends the
        // link for both ends.
        let (status, _, rejected) = fabric.reject(&peer_id, &link_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the peer end's RejectLink to answer 200, got {status} {rejected:?}"
        );
        assert_eq!(
            text(&rejected, "status"),
            LINK_STATUS_REJECTED,
            "case {case:?}: expected the declined link to be REJECTED, got {:?}",
            rejected["status"]
        );
        for (end, gateway_id) in [("opening end", &opener_id), ("peer end", &peer_id)] {
            let reading = fabric.read_link(case, gateway_id, &link_id).await;
            assert_eq!(
                text(&reading, "status"),
                LINK_STATUS_REJECTED,
                "case {case:?}: expected the {end} to read REJECTED back, got {:?}",
                reading["status"]
            );
        }

        // And the opening end's decline is no more its own to give once the peer
        // has declined than it was while the request was outstanding.
        let (status, code, refused) = fabric.reject(&opener_id, &link_id).await;
        assert_refused(
            case,
            status,
            code.as_str(),
            &refused,
            [link_id.as_str(), opener_id.as_str(), peer_id.as_str()],
        );
    }
}

#[tokio::test]
async fn the_opening_end_cannot_decline_a_link_in_any_status() {
    // The whole modelled catalog, from `LINK_STATUS_VALUES` (this service's
    // `LinkStatus` enum), so a status no operation reaches yet is covered too.
    for status_value in LINK_STATUS_VALUES {
        let case = *status_value;
        let fabric = Fabric::new();
        let (requester_id, responder_id) = fabric.gateway_pair(case, "reject-end").await;
        let link_id = fabric.open_link(case, &requester_id, &responder_id).await;
        fabric.force_link_status(status_value);

        let (status, code, body) = fabric.reject(&requester_id, &link_id).await;
        assert_refused(
            case,
            status,
            code.as_str(),
            &body,
            [
                link_id.as_str(),
                requester_id.as_str(),
                responder_id.as_str(),
            ],
        );

        // The refused call left the link exactly as it was.
        let reading = fabric.read_link(case, &requester_id, &link_id).await;
        assert_eq!(
            text(&reading, "status"),
            case,
            "case {case:?}: expected the refused reject to leave the status alone, got {:?}",
            reading["status"]
        );
    }
}
