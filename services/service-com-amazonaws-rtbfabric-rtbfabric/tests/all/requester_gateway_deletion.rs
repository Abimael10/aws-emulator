// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the requester gateway record after its create:
//! `UpdateRequesterGateway`, which changes it, and `DeleteRequesterGateway`, which
//! releases it.
//!
//! The rule under test is about state, not about one recorded request: the delete releases
//! everything the store holds BECAUSE of that gateway — the gateway record, every link it
//! was an end of, and the client-token records that minted it — and refuses an id it does
//! not hold with the modelled not-found. The table spans what the rule generalises over:
//! how much the gateway carried (no links, a link to a peer, several gateways of which one
//! is deleted) and whether the id was ever stored.
//!
//! The update's own rules are stated with its section, at the end of this file.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::REQUESTER_GATEWAY_STATUS_VALUES;
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// The one emulated service, behind the same `MultiServiceRouter` the production router
/// assembles.
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

    /// Create one requester gateway and return the id the service minted.
    async fn create_requester(&self, case: &str, client_token: &str) -> String {
        let (status, body) = self
            .send("POST", "/requester-gateway", Some(requester_body(client_token)))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {body:?}"
        );
        text(&body, "gatewayId")
    }

    /// Create one responder gateway and return the id the service minted.
    async fn create_responder(&self, case: &str, client_token: &str) -> String {
        let (status, body) = self
            .send("POST", "/responder-gateway", Some(responder_body(client_token)))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {status} {body:?}"
        );
        text(&body, "gatewayId")
    }

    /// The ids `ListRequesterGateways` reports, sorted.
    async fn listed(&self, case: &str) -> Vec<String> {
        let (status, body) = self.send("GET", "/requester-gateways", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListRequesterGateways to answer 200, got {status} {body:?}"
        );
        let mut ids: Vec<String> = body["gatewayIds"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected gatewayIds to be a list, got {body:?}"))
            .iter()
            .map(|id| id.as_str().unwrap_or_default().to_string())
            .collect();
        ids.sort();
        ids
    }
}

/// A `CreateRequesterGateway` body whose members satisfy the modelled patterns.
fn requester_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "clientToken": client_token,
    })
}

/// A `CreateResponderGateway` body, for the peer end of a link.
fn responder_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "port": 443,
        "protocol": "HTTPS",
        "listenerConfig": { "protocols": ["HTTPS"] },
        "domainName": "responder.example.com",
        "clientToken": client_token,
    })
}

/// A `CreateLink` body joining a stored requester gateway to `peer`. `logSettings` carries
/// the members `smithy_query(query="shape", shape="LinkLogSettings,\
/// LinkApplicationLogConfiguration")` marks required.
fn link_body(peer: &str) -> Value {
    json!({
        "peerGatewayId": peer,
        "logSettings": {
            "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } }
        },
    })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// One row of the release table: how many requester gateways the store holds and whether
/// the one being deleted is an end of a link.
struct Row {
    name: &'static str,
    stored: usize,
    linked: bool,
}

const ROWS: &[Row] = &[
    Row {
        name: "the only gateway, carrying nothing",
        stored: 1,
        linked: false,
    },
    Row {
        name: "one of several gateways",
        stored: 4,
        linked: false,
    },
    Row {
        name: "a gateway that is an end of a link",
        stored: 1,
        linked: true,
    },
    Row {
        name: "one of several gateways, and the linked one",
        stored: 3,
        linked: true,
    },
];

#[tokio::test]
async fn the_delete_releases_the_gateway_and_everything_held_because_of_it() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();

        let mut created: Vec<String> = Vec::new();
        for n in 0..row.stored {
            created.push(fabric.create_requester(case, &format!("token-{n}")).await);
        }
        let deleted = created[0].clone();

        let mut link_id = String::new();
        let mut peer = String::new();
        if row.linked {
            peer = fabric.create_responder(case, "token-peer").await;
            let (status, link) = fabric
                .send(
                    "POST",
                    &format!("/gateway/{deleted}/create-link"),
                    Some(link_body(&peer)),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateLink to answer 200, got {status} {link:?}"
            );
            link_id = text(&link, "linkId");
        }

        let (status, answered) = fabric
            .send("DELETE", &format!("/requester-gateway/{deleted}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete to answer 200, got {status} {answered:?}"
        );
        assert_eq!(
            text(&answered, "gatewayId"),
            deleted,
            "case {case:?}: expected the delete to answer the gateway it released, got {answered:?}"
        );
        assert_eq!(
            text(&answered, "status"),
            "DELETED",
            "case {case:?}: expected a modelled terminal status, got {answered:?}"
        );

        // The reader no longer resolves it, and the listing no longer reports it.
        let (status, read) = fabric
            .send("GET", &format!("/requester-gateway/{deleted}"), None)
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the released gateway to be unreadable, got {status} {read:?}"
        );
        let mut survivors: Vec<String> = created[1..].to_vec();
        survivors.sort();
        assert_eq!(
            fabric.listed(case).await,
            survivors,
            "case {case:?}: expected only the gateways that were not deleted to be listed"
        );

        if row.linked {
            // The link cannot outlive either of its two ends, so it is gone from the
            // peer's side too — while the peer gateway itself survives.
            let (status, read) = fabric
                .send("GET", &format!("/gateway/{peer}/link/{link_id}"), None)
                .await;
            assert_eq!(
                status, 404,
                "case {case:?}: expected the link to be released with its end, got {status} {read:?}"
            );
            let (status, read) = fabric
                .send("GET", &format!("/responder-gateway/{peer}"), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the peer gateway to survive, got {status} {read:?}"
            );
            assert_eq!(
                read["totalLinksCount"],
                json!(0),
                "case {case:?}: expected the peer to count no links, got {read:?}"
            );
        }

        // The client token that minted the released gateway is forgotten with it: a
        // retry mints a new gateway rather than replaying an id the store no longer
        // holds.
        let reused = fabric.create_requester(case, "token-0").await;
        assert_ne!(
            reused, deleted,
            "case {case:?}: expected the retry to mint a new gateway, got the released id"
        );
        let (status, read) = fabric
            .send("GET", &format!("/requester-gateway/{reused}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the newly minted gateway to resolve, got {status} {read:?}"
        );
    }
}

#[tokio::test]
async fn an_id_the_store_does_not_hold_is_refused_with_the_modelled_not_found() {
    let fabric = Fabric::new();
    let stored = fabric.create_requester("not found", "token-0").await;
    // A responder gateway is a different resource: its id is not a requester gateway's.
    let responder = fabric.create_responder("not found", "token-peer").await;

    let (status, _) = fabric
        .send("DELETE", &format!("/requester-gateway/{stored}"), None)
        .await;
    assert_eq!(status, 200, "expected the first delete to answer 200");

    // Every id below satisfies the modelled `^rtb-gw-[a-z0-9-]{1,25}$`, so the refusal is
    // the handler's not-found rather than a constraint refusal.
    for (case, gateway_id) in [
        ("the gateway just released", stored),
        ("a responder gateway's id", responder),
        ("an id never minted", "rtb-gw-0000000000000000".to_string()),
    ] {
        let (status, body) = fabric
            .send("DELETE", &format!("/requester-gateway/{gateway_id}"), None)
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the modelled not-found, got {status} {body:?}"
        );
        assert_eq!(
            text(&body, "__type"),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled error code, got {body:?}"
        );
        assert_eq!(
            text(&body, "message"),
            "The requested resource was not found.",
            "case {case:?}: expected the fixed not-found wording, got {body:?}"
        );
    }
}

#[tokio::test]
async fn a_gateway_still_settling_can_be_released() {
    let fabric = Fabric::new();
    let gateway_id = fabric.create_requester("still settling", "token-0").await;

    // Straight after the create the gateway is still in its transient status, and the
    // release does not wait for the transition the clock owes.
    let (status, read) = fabric
        .send("GET", &format!("/requester-gateway/{gateway_id}"), None)
        .await;
    assert_eq!(status, 200, "expected the gateway to resolve, got {read:?}");
    assert_eq!(
        text(&read, "status"),
        "PENDING_CREATION",
        "expected the transient status straight after the create, got {read:?}"
    );

    let (status, answered) = fabric
        .send("DELETE", &format!("/requester-gateway/{gateway_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected a pending gateway to be releasable, got {status} {answered:?}"
    );

    // The release holds once the clock has moved on: a gateway created after it settles
    // ACTIVE, so the instant at which the released gateway's own pending transition would
    // have come due has passed, and the released gateway is still gone. The deadline
    // bounds a hang, it does not time the transition.
    let other = fabric.create_requester("still settling", "token-1").await;
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        let (status, read) = fabric
            .send("GET", &format!("/requester-gateway/{other}"), None)
            .await;
        assert_eq!(
            status, 200,
            "expected the second gateway to keep resolving while it settles, got {read:?}"
        );
        if text(&read, "status") == "ACTIVE" {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected the second gateway to settle ACTIVE, still {:?}",
            read["status"]
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (status, read) = fabric
        .send("GET", &format!("/requester-gateway/{gateway_id}"), None)
        .await;
    assert_eq!(
        status, 404,
        "expected the released gateway to stay gone, got {status} {read:?}"
    );
}

// ---------------------------------------------------------------------------
// `UpdateRequesterGateway`: changing the stored requester gateway record.
//
// The rule under test is about state, not about one recorded request: the update
// applies the members the caller STATES to the stored record — one left out is not
// a request to clear what is stored — and reports that record's OWN status, which
// is pending while the gateway puts the change into service. A gateway that is not
// in service has no configuration in service to change, so it is refused. A retry
// of the `clientToken` the record last took is answered with the record rather than
// applied to it a second time, and the same token carrying different parameters is
// refused instead of answering for parameters the caller did not send. The table
// spans what the rules generalise over: whether the record already carried a
// description, whether the update states one, and the form and length of the one it
// states.
// ---------------------------------------------------------------------------

/// The status a gateway in service reports while a change it has taken is being put
/// into service.
const UPDATING_STATUS: &str = "PENDING_UPDATE";

/// The status the clock owes a gateway once provisioning, or a change it took, is
/// due.
const IN_SERVICE_STATUS: &str = "ACTIVE";

/// The status a create leaves on a gateway that has not come up yet.
const COMING_UP_STATUS: &str = "PENDING_CREATION";

/// How long a wait for a status the clock owes gives up after. Generous: it bounds a
/// hang, it does not time the transition.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long such a wait pauses between two looks at the record.
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// One update case: what the record carried and what the update states.
struct UpdateRow {
    name: &'static str,
    /// The description the create stored, if any.
    created: Option<&'static str>,
    /// The description the update states, if any. Leaving it out asks for no change
    /// to it, so the stored one survives.
    stated: Option<&'static str>,
}

/// Every description here satisfies the `^[A-Za-z0-9 ]+$` the model gives
/// `UpdateRequesterGatewayRequest$description`
/// (`smithy_query(query="operation", operation="UpdateRequesterGateway")`), so the
/// refusal of a description OUTSIDE that pattern is tested on its own below.
const UPDATE_ROWS: &[UpdateRow] = &[
    UpdateRow {
        name: "a description set on a gateway that carried none",
        created: None,
        stated: Some("Spot bidding fabric"),
    },
    UpdateRow {
        name: "a description replacing the one the create stored",
        created: Some("Display exchange"),
        stated: Some("Display exchange 2"),
    },
    UpdateRow {
        name: "the shortest description the pattern admits",
        created: Some("Display exchange"),
        stated: Some("A"),
    },
    UpdateRow {
        name: "a long description of letters digits and spaces",
        created: None,
        stated: Some(
            "Spot bidding fabric 12345678901234567890 serving the display exchange and \
             the video exchange alike",
        ),
    },
    UpdateRow {
        name: "no description stated leaves the stored one alone",
        created: Some("Spot bidding fabric"),
        stated: None,
    },
    UpdateRow {
        name: "no description stated on a gateway that carried none",
        created: None,
        stated: None,
    },
];

impl UpdateRow {
    /// The description the record carries once the update has been applied: the one
    /// the update stated, or the stored one when it stated none.
    fn expected_description(&self) -> Option<&'static str> {
        self.stated.or(self.created)
    }
}

/// A `CreateRequesterGateway` body that stores `description` when one is given.
fn described_requester_body(description: Option<&str>, client_token: &str) -> Value {
    let mut body = requester_body(client_token);
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

/// An `UpdateRequesterGateway` body: the required `clientToken`, plus `description`
/// only when the caller states one.
fn update_requester_body(description: Option<&str>, client_token: &str) -> Value {
    let mut body = json!({ "clientToken": client_token });
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

impl Fabric {
    /// Create one requester gateway carrying `description` and return the id the
    /// service minted.
    async fn create_described_requester(
        &self,
        case: &str,
        description: Option<&str>,
        client_token: &str,
    ) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/requester-gateway",
                Some(described_requester_body(description, client_token)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {body:?}"
        );
        text(&body, "gatewayId")
    }

    /// `GET /requester-gateway/{gatewayId}`, checked to answer 200.
    async fn read_requester(&self, case: &str, gateway_id: &str) -> Value {
        let (status, body) = self
            .send("GET", &format!("/requester-gateway/{gateway_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetRequesterGateway to answer 200, got {status} {body:?}"
        );
        body
    }

    /// Read the gateway until it reports `expected`, and answer with that reading.
    ///
    /// Waits for the CONDITION rather than for a span: it looks again until the
    /// status holds, giving up only once [`SETTLE_DEADLINE`] has passed.
    async fn poll_requester_until(&self, case: &str, gateway_id: &str, expected: &str) -> Value {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let read = self.read_requester(case, gateway_id).await;
            if text(&read, "status") == expected {
                return read;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: expected the gateway to reach {expected:?}, got {:?}",
                read["status"]
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// `POST /requester-gateway/{gatewayId}/update`.
    async fn update_requester(&self, gateway_id: &str, body: Value) -> (u16, Value) {
        self.send(
            "POST",
            &format!("/requester-gateway/{gateway_id}/update"),
            Some(body),
        )
        .await
    }
}

#[tokio::test]
async fn an_update_applies_what_it_states_and_reports_the_record_status() {
    // Every status named here is a modelled `RequesterGatewayStatus` member, from
    // `smithy_query(query="shape", shape="RequesterGatewayStatus")`.
    for status in [UPDATING_STATUS, IN_SERVICE_STATUS, COMING_UP_STATUS] {
        assert!(
            REQUESTER_GATEWAY_STATUS_VALUES.contains(&status),
            "case {status:?}: expected a modelled status, got {REQUESTER_GATEWAY_STATUS_VALUES:?}"
        );
    }

    for row in UPDATE_ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_described_requester(case, row.created, "token-create")
            .await;

        // The change is applied to a gateway in service, which is the only status
        // that has a configuration in service to change.
        let before = fabric
            .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
            .await;

        let (status, answered) = fabric
            .update_requester(
                &gateway_id,
                update_requester_body(row.stated, "token-update"),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer 200, got {status} {answered:?}"
        );
        assert_eq!(
            text(&answered, "gatewayId"),
            gateway_id,
            "case {case:?}: expected the update to answer for the gateway it changed, got {answered:?}"
        );
        // The gateway puts the change into service, which is what the pending status
        // reports; the clock then owes it its way back.
        assert_eq!(
            text(&answered, "status"),
            UPDATING_STATUS,
            "case {case:?}: expected the update to report the record's own status, got {answered:?}"
        );

        // The change is in the record, and the members the update did not state are
        // exactly what the create stored.
        let settled = fabric
            .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
            .await;
        assert_eq!(
            settled["description"],
            json!(row.expected_description()),
            "case {case:?}: expected the stated description to be stored, got {settled:?}"
        );
        for member in [
            "gatewayId",
            "domainName",
            "vpcId",
            "subnetIds",
            "securityGroupIds",
            "createdAt",
            "activeLinksCount",
            "totalLinksCount",
        ] {
            assert_eq!(
                settled[member], before[member],
                "case {case:?}: expected the update to leave {member:?} alone, got {:?} after {:?}",
                settled[member], before[member]
            );
        }
        let created_at = epoch_seconds(&settled, "createdAt");
        let updated_at = epoch_seconds(&settled, "updatedAt");
        assert!(
            updated_at >= created_at,
            "case {case:?}: expected updatedAt {updated_at} not to precede createdAt {created_at}"
        );
    }
}

#[tokio::test]
async fn a_retry_of_the_token_the_record_last_took_is_answered_rather_than_applied_again() {
    let case = "a retried update";
    let fabric = Fabric::new();
    let gateway_id = fabric
        .create_described_requester(case, None, "token-create")
        .await;
    fabric
        .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
        .await;

    let body = update_requester_body(Some("Spot bidding fabric"), "token-update");
    let (status, first) = fabric.update_requester(&gateway_id, body.clone()).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the first update to answer 200, got {status} {first:?}"
    );
    assert_eq!(
        text(&first, "status"),
        UPDATING_STATUS,
        "case {case:?}: expected the change to be reported as pending, got {first:?}"
    );
    let settled = fabric
        .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
        .await;

    // The retry carries the same token and the same parameters, so it is answered
    // with the record that change left: the gateway is NOT put through a second
    // change, and nothing about it moves.
    let (status, replayed) = fabric.update_requester(&gateway_id, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the retry to answer 200, got {status} {replayed:?}"
    );
    assert_eq!(
        text(&replayed, "gatewayId"),
        gateway_id,
        "case {case:?}: expected the retry to answer for the same gateway, got {replayed:?}"
    );
    assert_eq!(
        text(&replayed, "status"),
        IN_SERVICE_STATUS,
        "case {case:?}: expected the retry to report the record's settled status, got {replayed:?}"
    );
    let after = fabric.read_requester(case, &gateway_id).await;
    assert_eq!(
        after, settled,
        "case {case:?}: expected the retry to leave the record as the first update left it, got {after:?}"
    );
}

#[tokio::test]
async fn a_gateway_that_is_not_in_service_is_refused_with_the_modelled_conflict() {
    let case = "still coming up";
    let fabric = Fabric::new();
    let gateway_id = fabric
        .create_described_requester(case, Some("Display exchange"), "token-create")
        .await;

    // Straight after the create the gateway has not come up, so it has no
    // configuration in service for the update to change.
    let coming_up = fabric.read_requester(case, &gateway_id).await;
    assert_eq!(
        text(&coming_up, "status"),
        COMING_UP_STATUS,
        "case {case:?}: expected the gateway to still be coming up, got {coming_up:?}"
    );

    let (status, refused) = fabric
        .update_requester(
            &gateway_id,
            update_requester_body(Some("Video exchange"), "token-update"),
        )
        .await;
    assert_eq!(
        status, 409,
        "case {case:?}: expected the modelled conflict, got {status} {refused:?}"
    );
    assert_eq!(
        text(&refused, "__type"),
        "ConflictException",
        "case {case:?}: expected the modelled error code, got {refused:?}"
    );
    assert_eq!(
        text(&refused, "message"),
        format!("requester gateway {gateway_id} is {COMING_UP_STATUS} and cannot be updated"),
        "case {case:?}: expected the refusal to name the gateway and the status that \
         refuses it, got {refused:?}"
    );

    // The refused change is nowhere in the record, and the gateway still comes up.
    let settled = fabric
        .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
        .await;
    assert_eq!(
        settled["description"],
        json!("Display exchange"),
        "case {case:?}: expected the refusal to change nothing, got {settled:?}"
    );

    // Once it is in service the same change is taken, so the refusal was about the
    // status and not about the request.
    let (status, answered) = fabric
        .update_requester(
            &gateway_id,
            update_requester_body(Some("Video exchange"), "token-update"),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the gateway in service to take the change, got {status} {answered:?}"
    );
    let applied = fabric
        .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
        .await;
    assert_eq!(
        applied["description"],
        json!("Video exchange"),
        "case {case:?}: expected the change to be stored once it was taken, got {applied:?}"
    );
}

/// One mismatch case: the parameters the first call under a token sent, and the
/// different ones a later call sends under the same token.
struct MismatchRow {
    name: &'static str,
    first: Option<&'static str>,
    second: Option<&'static str>,
}

const MISMATCH_ROWS: &[MismatchRow] = &[
    MismatchRow {
        name: "a different description",
        first: Some("Display exchange"),
        second: Some("Video exchange"),
    },
    MismatchRow {
        name: "a description where the first call stated none",
        first: None,
        second: Some("Video exchange"),
    },
    MismatchRow {
        name: "no description where the first call stated one",
        first: Some("Display exchange"),
        second: None,
    },
];

#[tokio::test]
async fn the_same_token_with_different_parameters_is_refused_and_changes_nothing() {
    for row in MISMATCH_ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_described_requester(case, Some("Spot bidding fabric"), "token-create")
            .await;
        fabric
            .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
            .await;

        let (status, first) = fabric
            .update_requester(&gateway_id, update_requester_body(row.first, "token-shared"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first update to answer 200, got {status} {first:?}"
        );
        let applied = fabric
            .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
            .await;

        let (status, refused) = fabric
            .update_requester(
                &gateway_id,
                update_requester_body(row.second, "token-shared"),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the modelled validation refusal, got {status} {refused:?}"
        );
        assert_eq!(
            text(&refused, "__type"),
            "ValidationException",
            "case {case:?}: expected the modelled error code, got {refused:?}"
        );
        assert_eq!(
            text(&refused, "message"),
            "clientToken token-shared was already used for a UpdateRequesterGateway request \
             with different parameters",
            "case {case:?}: expected the mismatch wording, got {refused:?}"
        );

        // The refused call is not half-applied: the record is exactly what the first
        // call under that token left.
        let after = fabric.read_requester(case, &gateway_id).await;
        assert_eq!(
            after, applied,
            "case {case:?}: expected the refusal to change nothing, got {after:?}"
        );
    }
}

#[tokio::test]
async fn an_update_of_an_id_the_store_does_not_hold_is_refused_with_the_modelled_not_found() {
    let case = "update not found";
    let fabric = Fabric::new();
    let released = fabric.create_requester(case, "token-released").await;
    let responder = fabric.create_responder(case, "token-responder").await;
    let (status, _) = fabric
        .send("DELETE", &format!("/requester-gateway/{released}"), None)
        .await;
    assert_eq!(status, 200, "expected the delete to answer 200");

    // Every id below satisfies the modelled `^rtb-gw-[a-z0-9-]{1,25}$`, so the refusal
    // is the handler's not-found rather than a constraint refusal.
    for (case, gateway_id) in [
        ("the gateway just released", released),
        ("a responder gateway's id", responder),
        ("an id never minted", "rtb-gw-0000000000000000".to_string()),
    ] {
        let (status, body) = fabric
            .update_requester(
                &gateway_id,
                update_requester_body(Some("Display exchange"), "token-update"),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the modelled not-found, got {status} {body:?}"
        );
        assert_eq!(
            text(&body, "__type"),
            "ResourceNotFoundException",
            "case {case:?}: expected the modelled error code, got {body:?}"
        );
        assert_eq!(
            text(&body, "message"),
            "The requested resource was not found.",
            "case {case:?}: expected the fixed not-found wording, got {body:?}"
        );
    }
}

#[tokio::test]
async fn an_update_outside_the_modelled_constraints_is_refused_and_changes_nothing() {
    let case = "update refused at the boundary";
    let fabric = Fabric::new();
    let gateway_id = fabric
        .create_described_requester(case, Some("Display exchange"), "token-create")
        .await;
    let stored = fabric
        .poll_requester_until(case, &gateway_id, IN_SERVICE_STATUS)
        .await;

    // `description` is `^[A-Za-z0-9 ]+$` and `clientToken` is required, both from
    // `smithy_query(query="operation", operation="UpdateRequesterGateway")`.
    let refused = [
        ("a hyphen", json!({ "clientToken": "token-a", "description": "display-exchange" })),
        ("punctuation", json!({ "clientToken": "token-b", "description": "Display exchange!" })),
        ("an empty description", json!({ "clientToken": "token-c", "description": "" })),
        ("a non-ASCII letter", json!({ "clientToken": "token-d", "description": "Café" })),
        ("no clientToken", json!({ "description": "Video exchange" })),
    ];
    for (why, body) in refused {
        let (status, answered) = fabric.update_requester(&gateway_id, body).await;
        assert_eq!(
            status, 400,
            "case {why:?}: expected the modelled validation refusal, got {status} {answered:?}"
        );
        assert_eq!(
            text(&answered, "__type"),
            "ValidationException",
            "case {why:?}: expected the modelled error code, got {answered:?}"
        );
        let after = fabric.read_requester(case, &gateway_id).await;
        assert_eq!(
            after, stored,
            "case {why:?}: expected the refused update to change nothing, got {after:?}"
        );
    }
}

/// One `@timestampFormat`-free modelled timestamp, as restJson1 renders it: epoch
/// seconds, so two readings of one record can be ordered.
fn epoch_seconds(body: &Value, member: &str) -> f64 {
    body[member]
        .as_f64()
        .unwrap_or_else(|| panic!("expected {member:?} to be an epoch-seconds number, got {body:?}"))
}
