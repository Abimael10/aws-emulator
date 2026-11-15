// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `AcceptVpcPeeringConnection`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rules under test:
//!
//! * An acceptance ANSWERS THE STORED REQUEST: the connection leaves the state it was
//!   waiting in for `active`, the state a peering that carries traffic is in, and it keeps no
//!   expiry — "The time that an unaccepted VPC peering connection will expire"
//!   (`VpcPeeringConnection$ExpirationTime`). `DescribeVpcPeeringConnections` reads every one
//!   of those facts back, including the accepter VPC's address space, which is reported once
//!   the connection is active. The rows differ in the tags the request carried, the address
//!   families the two VPCs hold and whether the two sides are in one Region.
//! * "To accept a request, the VPC peering connection must be in the pending-acceptance
//!   state" (`AcceptVpcPeeringConnection`): over the WHOLE modelled
//!   `VpcPeeringConnectionStateReasonCode` catalog, a request that is still waiting — one on
//!   its way in included, so an acceptance issued straight after the create is served — is
//!   recorded as accepted; a connection whose acceptance was already recorded is answered as
//!   it stands with nothing written; and a peering that is no longer one at all is refused.
//! * "To accept a request, … you must be the owner of the peer VPC", and "For an
//!   inter-Region VPC peering connection request, you must accept the VPC peering connection
//!   in the Region of the accepter VPC" (`AcceptVpcPeeringConnection`): a call for another
//!   account's accepter, or one made outside the accepter's Region, is refused and the
//!   request is left waiting.
//! * An id that is no peering connection id, one this account holds no connection for, and a
//!   dry run are each refused and write nothing.
//!
//! Every wait here is a POLL for the condition — the state the clock owes the request — with
//! a deadline far past the service's own transition span, so nothing depends on how fast the
//! host runs.

use std::time::Duration;

use ec2::ops::types::{
    VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES, VpcPeeringConnectionStateReason,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_STATE_TRANSITION, TRANSITION_DELAY,
    VPC_PEERING_CONNECTION_ACTIVE_MESSAGE, VPC_PEERING_CONNECTION_ID_PREFIX,
    VPC_PEERING_CONNECTION_STATE_ACTIVE, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
    vpc_peering_connection_awaits_acceptance, vpc_peering_connection_holds_its_pair,
    vpc_peering_pending_acceptance_message,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message, indexed};
use super::wire::{child, child_text, document, set_items};

/// The Region every call here is signed for unless the case says otherwise: the requester
/// VPC is in the Region the request was made in.
const REGION: &str = "us-east-1";

/// A Region of the same partition that is NOT [`REGION`], for the accepter side of an
/// inter-Region request (`CreateVpcPeeringConnectionRequest$PeerRegion`).
const OTHER_REGION: &str = "us-west-2";

/// An account that is not the one this emulator serves, for a cross-account accepter —
/// twelve digits, as an account id is.
const OTHER_ACCOUNT: &str = "210987654321";

/// A well-formed VPC id of [`OTHER_ACCOUNT`], which this store therefore holds no VPC for.
const OTHER_ACCOUNT_VPC_ID: &str = "vpc-0a1b2c3d4e5f60718";

/// The deadline every poll below gives up at: a large multiple of the service's own
/// transition span, so a loaded host can never be the reason a state has not moved yet.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// One look inside that deadline, short enough that the deadline costs many looks rather
/// than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One VPC created over the wire, by id.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str, ipv6: bool) -> String {
    let mut params = vec![param("CidrBlock", cidr_block)];
    if ipv6 {
        params.push(param("AmazonProvidedIpv6CidrBlock", "true"));
    }
    let (status, body) = call(router, REGION, "CreateVpc", &params).await;
    assert_eq!(
        status, 200,
        "CreateVpc {cidr_block}: expected HTTP 200, got {status} with body {body}"
    );
    super::wire::member(&format!("CreateVpc {cidr_block}"), &body, "vpcId")
}

/// One peering connection as a response reported it, read through the XML tree so a member
/// of one side is never confused with the identically-named member of the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Reported {
    id: String,
    status_code: String,
    status_message: Option<String>,
    expiration_time: Option<String>,
    accepter_vpc_id: String,
    accepter_region: String,
    accepter_owner_id: String,
    accepter_ipv4: Vec<String>,
    accepter_ipv6: Vec<String>,
    requester_vpc_id: String,
    requester_ipv4: Vec<String>,
    tags: Vec<(String, String)>,
}

/// The ONE connection `body` carries, or a panic naming the case.
fn reported(case: &str, body: &str) -> Reported {
    let parsed = document(case, body);
    let nodes: Vec<roxmltree::Node<'_, '_>> = parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "vpcPeeringConnectionId").is_some())
        .collect();
    assert_eq!(
        nodes.len(),
        1,
        "case {case:?}: expected exactly one peering connection, got {nodes:?} in {body}"
    );
    let node = nodes[0];
    let status = child(node, "status");
    let blocks = |side: &str, set: &str, member: &str| -> Vec<String> {
        child(node, side)
            .map(|info| {
                set_items(info, set)
                    .into_iter()
                    .filter_map(|item| child_text(item, member))
                    .collect()
            })
            .unwrap_or_default()
    };
    let text = |side: &str, member: &str| -> String {
        child(node, side)
            .and_then(|info| child_text(info, member))
            .unwrap_or_default()
    };
    Reported {
        id: child_text(node, "vpcPeeringConnectionId").unwrap_or_default(),
        status_code: status
            .and_then(|status| child_text(status, "code"))
            .unwrap_or_default(),
        status_message: status.and_then(|status| child_text(status, "message")),
        expiration_time: child_text(node, "expirationTime"),
        accepter_vpc_id: text("accepterVpcInfo", "vpcId"),
        accepter_region: text("accepterVpcInfo", "region"),
        accepter_owner_id: text("accepterVpcInfo", "ownerId"),
        accepter_ipv4: blocks("accepterVpcInfo", "cidrBlockSet", "cidrBlock"),
        accepter_ipv6: blocks("accepterVpcInfo", "ipv6CidrBlockSet", "ipv6CidrBlock"),
        requester_vpc_id: text("requesterVpcInfo", "vpcId"),
        requester_ipv4: blocks("requesterVpcInfo", "cidrBlockSet", "cidrBlock"),
        tags: set_items(node, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "key").unwrap_or_default(),
                    child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect(),
    }
}

/// The connection `id` names, read back through `DescribeVpcPeeringConnections`.
async fn described(router: &MultiServiceRouter, case: &str, id: &str) -> Reported {
    let params = indexed("VpcPeeringConnectionId", &[id]);
    let (status, body) = call(router, REGION, "DescribeVpcPeeringConnections", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} with body {body}"
    );
    reported(case, &body)
}

/// Look at the connection AGAIN until the describe reports it in `wanted`, or a deadline far
/// past the service's own transition span passes. Answers the connection as last seen, so
/// the caller asserts on a value rather than on the loop.
async fn polled(router: &MultiServiceRouter, case: &str, id: &str, wanted: &str) -> Reported {
    let deadline = tokio::time::Instant::now() + poll_deadline();
    loop {
        let seen = described(router, case, id).await;
        if seen.status_code == wanted {
            return seen;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected the connection to reach {wanted:?}, last saw {seen:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One `AcceptVpcPeeringConnection` call signed for `region`.
async fn accept_in(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![param("VpcPeeringConnectionId", id)];
    params.extend_from_slice(extra);
    call(router, region, "AcceptVpcPeeringConnection", &params).await
}

/// One `AcceptVpcPeeringConnection` call signed for [`REGION`].
async fn accept(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    accept_in(router, REGION, id, &[]).await
}

/// One peering request between two VPCs of this account, created over the wire and polled
/// until it is waiting for an answer: the starting point of most rows below.
async fn waiting_request(
    router: &MultiServiceRouter,
    case: &str,
    requester_cidr: &str,
    accepter_cidr: &str,
) -> String {
    let requester_vpc_id = create_vpc(router, requester_cidr, false).await;
    let accepter_vpc_id = create_vpc(router, accepter_cidr, false).await;
    let params = [
        param("VpcId", &requester_vpc_id),
        param("PeerVpcId", &accepter_vpc_id),
    ];
    let (status, body) = call(router, REGION, "CreateVpcPeeringConnection", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from the create, got {status} with body {body}"
    );
    let id = reported(case, &body).id;
    polled(
        router,
        case,
        &id,
        VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
    )
    .await;
    id
}

/// The form fields that ask a create for `tags` on the connection it records.
fn tag_params(tags: &[(&str, &str)]) -> Vec<(String, String)> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        "vpc-peering-connection",
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        params.push(param(
            &format!("TagSpecification.1.Tag.{position}.Key"),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{position}.Value"),
            value,
        ));
    }
    params
}

/// The status the store holds for `id`, as `(code, message)`.
fn stored_status(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    id: &str,
) -> (String, Option<String>) {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    let record = guard
        .vpc_peering_connections
        .get(id)
        .expect("the connection under test is stored");
    let status = record.view.status.clone().unwrap_or_default();
    (status.code.unwrap_or_default(), status.message)
}

/// Write `code` onto the stored connection `id`, leaving the clock owing it nothing: the way
/// this suite reaches a state of the modelled catalog that no operation of this crate mints
/// yet (`rejected`, `deleted`), so the rule is asserted over the WHOLE catalog rather than
/// only the states a create and an acceptance happen to produce.
fn seed_status(engine: &std::sync::Arc<engine::InMemoryEngine>, id: &str, code: &str) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let record = guard
        .vpc_peering_connections
        .get_mut(id)
        .expect("the connection under test is stored");
    record.view.status = Some(VpcPeeringConnectionStateReason {
        code: Some(code.to_string()),
        message: None,
    });
    record.pending = None;
}

// ---------------------------------------------------------------------------
// The acceptance answers the stored request
// ---------------------------------------------------------------------------

/// One row of the acceptance table: which pair of VPCs, where the accepter is and what the
/// request carried.
struct AcceptCase {
    name: &'static str,
    requester_cidr: &'static str,
    accepter_cidr: &'static str,
    /// Whether the accepter VPC also holds an Amazon-provided IPv6 block.
    accepter_ipv6: bool,
    /// The Region the request names for the accepter VPC, if any — and therefore the Region
    /// the acceptance has to be made in.
    peer_region: Option<&'static str>,
    tags: Vec<(&'static str, &'static str)>,
}

/// Each acceptance answers the request the store holds: the connection is reported `active`
/// with the message that state carries and no expiry left, both sides' address space is
/// reported — "CIDR block information is only returned when describing an active VPC peering
/// connection" (`VpcPeeringConnection$AccepterVpcInfo`) — and
/// `DescribeVpcPeeringConnections` reads back exactly what the acceptance answered. The rows
/// differ in the tags the request carried, the address families the accepter holds and
/// whether the two sides are in one Region.
#[tokio::test]
async fn an_accepted_request_is_active_and_round_trips_through_the_describe() {
    let (router, engine) = super::ec2_query::emulator();
    let cases = vec![
        AcceptCase {
            name: "one Region, no tags",
            requester_cidr: "10.1.0.0/16",
            accepter_cidr: "10.2.0.0/16",
            accepter_ipv6: false,
            peer_region: None,
            tags: Vec::new(),
        },
        AcceptCase {
            name: "one Region, two tags",
            requester_cidr: "10.3.0.0/16",
            accepter_cidr: "10.4.0.0/24",
            accepter_ipv6: false,
            peer_region: None,
            tags: vec![("Name", "east-west"), ("Owner", "TeamA")],
        },
        AcceptCase {
            name: "a dual-stack accepter",
            requester_cidr: "10.5.0.0/16",
            accepter_cidr: "10.6.0.0/16",
            accepter_ipv6: true,
            peer_region: None,
            tags: vec![("Note", "zone A/B, primary")],
        },
        AcceptCase {
            name: "an inter-Region request, accepted in the accepter's Region",
            requester_cidr: "10.7.0.0/16",
            accepter_cidr: "10.8.0.0/16",
            accepter_ipv6: false,
            peer_region: Some(OTHER_REGION),
            tags: Vec::new(),
        },
    ];

    for case in cases {
        let name = case.name;
        let requester_vpc_id = create_vpc(&router, case.requester_cidr, false).await;
        let accepter_vpc_id = create_vpc(&router, case.accepter_cidr, case.accepter_ipv6).await;
        let mut params = vec![
            param("VpcId", &requester_vpc_id),
            param("PeerVpcId", &accepter_vpc_id),
        ];
        if let Some(peer_region) = case.peer_region {
            params.push(param("PeerRegion", peer_region));
        }
        params.extend(tag_params(&case.tags));
        let (status, body) = call(&router, REGION, "CreateVpcPeeringConnection", &params).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the create, got {status} with body {body}"
        );
        let requested = reported(name, &body);
        assert!(
            requested.id.starts_with(VPC_PEERING_CONNECTION_ID_PREFIX),
            "case {name:?}: expected a peering connection id, got {requested:?}"
        );

        // The request reaches the state in which it waits for an answer whether or not
        // anybody calls, and that is where a client that polls for it picks it up.
        let waiting = polled(
            &router,
            name,
            &requested.id,
            VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
        )
        .await;
        assert_eq!(
            waiting.status_message,
            Some(vpc_peering_pending_acceptance_message(
                engine::emulator_account_id()
            )),
            "case {name:?}: expected the waiting request to name the account it waits for, \
             got {waiting:?}"
        );
        assert!(
            waiting.expiration_time.is_some(),
            "case {name:?}: an unanswered request carries the instant it expires at, got \
             {waiting:?}"
        );
        assert!(
            waiting.accepter_ipv4.is_empty(),
            "case {name:?}: the accepter's ranges are reported only once the connection is \
             active, got {waiting:?}"
        );

        // The acceptance itself, made in the Region the accepter VPC is in.
        let accept_region = case.peer_region.unwrap_or(REGION);
        let (status, body) = accept_in(&router, accept_region, &requested.id, &[]).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the acceptance, got {status} with body \
             {body}"
        );
        let answered = reported(name, &body);
        assert_eq!(
            answered.id, requested.id,
            "case {name:?}: the acceptance answers the connection it was asked about, got \
             {answered:?}"
        );
        assert_eq!(
            (
                answered.status_code.as_str(),
                answered.status_message.as_deref()
            ),
            (
                VPC_PEERING_CONNECTION_STATE_ACTIVE,
                Some(VPC_PEERING_CONNECTION_ACTIVE_MESSAGE)
            ),
            "case {name:?}: expected the answered request to carry traffic, got {answered:?}"
        );
        assert_eq!(
            answered.expiration_time, None,
            "case {name:?}: an answered request has nothing left to expire, got {answered:?}"
        );
        assert_eq!(
            answered.accepter_region,
            accept_region.to_string(),
            "case {name:?}: expected the accepter's Region to be kept, got {answered:?}"
        );
        assert_eq!(
            answered.accepter_owner_id,
            engine::emulator_account_id(),
            "case {name:?}: expected the accepter's owner to be kept, got {answered:?}"
        );
        assert_eq!(
            (
                answered.requester_vpc_id.as_str(),
                answered.accepter_vpc_id.as_str()
            ),
            (requester_vpc_id.as_str(), accepter_vpc_id.as_str()),
            "case {name:?}: expected the two VPCs the request named, got {answered:?}"
        );
        assert_eq!(
            answered.requester_ipv4,
            vec![case.requester_cidr.to_string()],
            "case {name:?}: expected the requester's ranges, got {answered:?}"
        );
        assert_eq!(
            answered.accepter_ipv4,
            vec![case.accepter_cidr.to_string()],
            "case {name:?}: an active connection reports the accepter's ranges too, got \
             {answered:?}"
        );
        assert_eq!(
            answered.accepter_ipv6.len(),
            usize::from(case.accepter_ipv6),
            "case {name:?}: expected the accepter's IPv6 ranges to be reported exactly when \
             it holds one, got {answered:?}"
        );
        assert_eq!(
            answered.tags,
            case.tags
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<Vec<(String, String)>>(),
            "case {name:?}: expected the tags the request carried, got {answered:?}"
        );

        // The round trip: the describe reports exactly what the acceptance answered, so the
        // two cannot disagree about any of it.
        let read_back = described(&router, name, &requested.id).await;
        assert_eq!(
            read_back, answered,
            "case {name:?}: expected the describe to report the accepted connection \
             unchanged, got {read_back:?}"
        );
        let (code, message) = stored_status(&engine, &requested.id);
        assert_eq!(
            (code.as_str(), message.as_deref()),
            (
                VPC_PEERING_CONNECTION_STATE_ACTIVE,
                Some(VPC_PEERING_CONNECTION_ACTIVE_MESSAGE)
            ),
            "case {name:?}: expected the store to hold the active connection"
        );
    }
}

/// An acceptance issued STRAIGHT AFTER the create — with nothing in between, as a client
/// that requests a peering of its own VPC does — is served, whatever state the request has
/// reached by the time it arrives: the state the create answers is on its way to the one
/// that waits for an answer, and both are requests still waiting.
#[tokio::test]
async fn an_acceptance_issued_straight_after_the_create_is_served() {
    let (router, _engine) = super::ec2_query::emulator();
    let requester_vpc_id = create_vpc(&router, "10.50.0.0/16", false).await;
    let accepter_vpc_id = create_vpc(&router, "10.51.0.0/16", false).await;
    let params = [
        param("VpcId", &requester_vpc_id),
        param("PeerVpcId", &accepter_vpc_id),
    ];
    let (status, body) = call(&router, REGION, "CreateVpcPeeringConnection", &params).await;
    assert_eq!(
        status, 200,
        "the request: expected HTTP 200, got {status} with body {body}"
    );
    let requested = reported("the request", &body);

    let (status, body) = accept(&router, &requested.id).await;
    assert_eq!(
        status, 200,
        "the acceptance: expected HTTP 200, got {status} with body {body}"
    );
    let answered = reported("the acceptance", &body);
    assert_eq!(
        (
            answered.status_code.as_str(),
            answered.status_message.as_deref()
        ),
        (
            VPC_PEERING_CONNECTION_STATE_ACTIVE,
            Some(VPC_PEERING_CONNECTION_ACTIVE_MESSAGE)
        ),
        "the acceptance: expected the request to be answered, got {answered:?}"
    );
    assert_eq!(
        answered.accepter_ipv4,
        vec!["10.51.0.0/16".to_string()],
        "the acceptance: expected the active connection to report the accepter's ranges, \
         got {answered:?}"
    );
}

// ---------------------------------------------------------------------------
// Which connections an acceptance can be asked of
// ---------------------------------------------------------------------------

/// "To accept a request, the VPC peering connection must be in the pending-acceptance state"
/// (`AcceptVpcPeeringConnection`). Every state of the modelled
/// `VpcPeeringConnectionStateReasonCode` catalog
/// (`smithy_query(query="shape",shape="VpcPeeringConnectionStateReasonCode")`, exposed as
/// [`VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES`]) is a row:
///
/// * a request still WAITING — the one on its way in included — is recorded as accepted;
/// * a connection whose acceptance was ALREADY recorded is answered as it stands, and
///   nothing is written: the state it is in, its expiry and its ranges are all left where
///   they were;
/// * a peering that is no longer one at all is refused, and the state it is in is left
///   alone.
#[tokio::test]
async fn an_acceptance_is_served_for_a_live_peering_and_refused_for_a_dead_one() {
    let (router, engine) = super::ec2_query::emulator();
    let id = waiting_request(&router, "the catalog", "10.20.0.0/16", "10.21.0.0/16").await;

    let mut served = 0;
    let mut refused = 0;
    for code in VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES.iter().copied() {
        seed_status(&engine, &id, code);
        let before = described(&router, code, &id).await;
        let (status, body) = accept(&router, &id).await;

        if !vpc_peering_connection_holds_its_pair(Some(code)) {
            refused += 1;
            assert_eq!(
                status, 400,
                "case {code:?}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                INVALID_STATE_TRANSITION,
                "case {code:?}: expected the state-transition refusal, got {body}"
            );
            assert!(
                error_message(&body).contains(&id),
                "case {code:?}: expected the refusal to name the connection, got {body}"
            );
            let after = described(&router, code, &id).await;
            assert_eq!(
                after, before,
                "case {code:?}: a refused acceptance must leave the connection it found, got \
                 {after:?}"
            );
            continue;
        }

        served += 1;
        assert_eq!(
            status, 200,
            "case {code:?}: expected HTTP 200, got {status} with body {body}"
        );
        let answered = reported(code, &body);
        let after = described(&router, code, &id).await;
        assert_eq!(
            after, answered,
            "case {code:?}: expected the describe to report what the acceptance answered, \
             got {after:?}"
        );

        if vpc_peering_connection_awaits_acceptance(Some(code)) {
            assert_eq!(
                (
                    answered.status_code.as_str(),
                    answered.status_message.as_deref()
                ),
                (
                    VPC_PEERING_CONNECTION_STATE_ACTIVE,
                    Some(VPC_PEERING_CONNECTION_ACTIVE_MESSAGE)
                ),
                "case {code:?}: a waiting request is recorded as accepted, got {answered:?}"
            );
            assert_eq!(
                answered.expiration_time, None,
                "case {code:?}: an answered request has nothing left to expire, got \
                 {answered:?}"
            );
        } else {
            assert_eq!(
                answered, before,
                "case {code:?}: a connection already accepted is answered as it stands and \
                 nothing is written, got {answered:?}"
            );
            let (stored, _message) = stored_status(&engine, &id);
            assert_eq!(
                stored, code,
                "case {code:?}: a second acceptance must not move the connection on"
            );
        }
    }
    assert!(
        served > 0 && refused > 0,
        "both halves of the rule need rows, got {served} served and {refused} refused"
    );
    assert_eq!(
        served + refused,
        VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES.len(),
        "every state of the catalog is a row"
    );
}

// ---------------------------------------------------------------------------
// Who may accept, and where
// ---------------------------------------------------------------------------

/// One row of the standing table: where the accepter VPC is and whose it is, where the
/// acceptance is made from, and what the refusal has to name — `None` when the acceptance is
/// one the operation permits.
struct StandingCase {
    name: &'static str,
    requester_cidr: &'static str,
    /// The accepter VPC's block when it is a VPC of this account; `None` for a VPC of
    /// [`OTHER_ACCOUNT`], which this store holds none of.
    accepter_cidr: Option<&'static str>,
    /// The Region the request names for the accepter VPC, if any.
    peer_region: Option<&'static str>,
    /// The Region the acceptance is signed for.
    accept_region: &'static str,
    refused_naming: Option<&'static str>,
}

/// "To accept a request, … you must be the owner of the peer VPC" and "For an inter-Region
/// VPC peering connection request, you must accept the VPC peering connection in the Region
/// of the accepter VPC" (`AcceptVpcPeeringConnection`): a connection whose accepter belongs
/// to another account, and one accepted outside the accepter's Region, are both refused with
/// `OperationNotPermitted` and left waiting, while the same request accepted where the
/// accepter is is answered.
#[tokio::test]
async fn an_acceptance_from_the_wrong_account_or_region_is_refused() {
    assert_ne!(
        OTHER_ACCOUNT,
        engine::emulator_account_id(),
        "the cross-account row needs an account id this emulator does not serve"
    );
    let (router, engine) = super::ec2_query::emulator();

    for case in [
        // The accepter VPC belongs to another account, so no caller of this emulator owns
        // it — and the two sides are in one Region, so the Region is not what is being
        // refused.
        StandingCase {
            name: "another account's accepter",
            requester_cidr: "10.30.0.0/16",
            accepter_cidr: None,
            peer_region: None,
            accept_region: REGION,
            refused_naming: Some(OTHER_ACCOUNT),
        },
        // The accepter VPC is in another Region, so an acceptance made where the REQUESTER
        // is cannot answer it.
        StandingCase {
            name: "an inter-Region request accepted in the requester's Region",
            requester_cidr: "10.31.0.0/16",
            accepter_cidr: Some("10.32.0.0/16"),
            peer_region: Some(OTHER_REGION),
            accept_region: REGION,
            refused_naming: Some(OTHER_REGION),
        },
        // The same request, accepted where the accepter is: permitted.
        StandingCase {
            name: "an inter-Region request accepted in the accepter's Region",
            requester_cidr: "10.33.0.0/16",
            accepter_cidr: Some("10.34.0.0/16"),
            peer_region: Some(OTHER_REGION),
            accept_region: OTHER_REGION,
            refused_naming: None,
        },
        // A request whose two sides are in one Region, accepted somewhere else: the rule
        // holds in that direction too.
        StandingCase {
            name: "a same-Region request accepted in another Region",
            requester_cidr: "10.35.0.0/16",
            accepter_cidr: Some("10.36.0.0/16"),
            peer_region: None,
            accept_region: OTHER_REGION,
            refused_naming: Some(REGION),
        },
    ] {
        let name = case.name;
        let requester_vpc_id = create_vpc(&router, case.requester_cidr, false).await;
        let mut params = vec![param("VpcId", &requester_vpc_id)];
        match case.accepter_cidr {
            Some(accepter_cidr) => {
                let accepter_vpc_id = create_vpc(&router, accepter_cidr, false).await;
                params.push(param("PeerVpcId", &accepter_vpc_id));
            }
            None => {
                params.push(param("PeerVpcId", OTHER_ACCOUNT_VPC_ID));
                params.push(param("PeerOwnerId", OTHER_ACCOUNT));
            }
        }
        if let Some(peer_region) = case.peer_region {
            params.push(param("PeerRegion", peer_region));
        }
        let (status, body) = call(&router, REGION, "CreateVpcPeeringConnection", &params).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the create, got {status} with body {body}"
        );
        let id = reported(name, &body).id;
        let waiting = polled(
            &router,
            name,
            &id,
            VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
        )
        .await;
        assert_eq!(
            waiting.status_code, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
            "case {name:?}: the request has to be waiting before this row means anything, \
             got {waiting:?}"
        );

        let (status, body) = accept_in(&router, case.accept_region, &id, &[]).await;
        let Some(refused_naming) = case.refused_naming else {
            assert_eq!(
                status, 200,
                "case {name:?}: expected HTTP 200, got {status} with body {body}"
            );
            assert_eq!(
                reported(name, &body).status_code,
                VPC_PEERING_CONNECTION_STATE_ACTIVE,
                "case {name:?}: expected the request to be answered, got {body}"
            );
            continue;
        };
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "OperationNotPermitted",
            "case {name:?}: expected the standing refusal, got {body}"
        );
        let message = error_message(&body);
        assert!(
            message.contains(refused_naming) && message.contains(&id),
            "case {name:?}: expected the refusal to name {refused_naming} and the \
             connection, got {body}"
        );
        let (stored, _message) = stored_status(&engine, &id);
        assert_eq!(
            stored, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
            "case {name:?}: a refused acceptance leaves the request waiting"
        );
    }
}

// ---------------------------------------------------------------------------
// What it refuses without writing anything
// ---------------------------------------------------------------------------

/// A value that is no peering connection id is refused as malformed, a well-formed id this
/// account holds no connection for as not found, and a dry run reports the outcome without
/// answering the request — "If you have the required permissions, the error response is
/// DryRunOperation" (`DryRun`).
#[tokio::test]
async fn an_unresolvable_id_or_a_dry_run_writes_nothing() {
    let (router, engine) = super::ec2_query::emulator();

    // Not an id of this kind: a prefix with nothing after it, a hex run of neither the
    // short nor the long length, non-hex digits, upper case, and the id of another resource
    // kind — "<prefix>-<8 or 17 lowercase hexadecimal digits>" is the form every EC2
    // resource id takes.
    for malformed in [
        "pcx",
        "pcx-",
        "pcx-0a1b2c",
        "pcx-0a1b2c3d4e5f6071",
        "pcx-0a1b2c3d4e5f6071z",
        "pcx-0A1B2C3D4E5F60718",
        OTHER_ACCOUNT_VPC_ID,
    ] {
        let (status, body) = accept(&router, malformed).await;
        assert_eq!(
            status, 400,
            "case {malformed:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVpcPeeringConnectionID.Malformed",
            "case {malformed:?}: expected the malformed-id refusal, got {body}"
        );
    }

    // Well formed, and this account holds no connection for it.
    for unknown in ["pcx-0a1b2c3d", "pcx-0a1b2c3d4e5f60718"] {
        let (status, body) = accept(&router, unknown).await;
        assert_eq!(
            status, 400,
            "case {unknown:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidVpcPeeringConnectionID.NotFound",
            "case {unknown:?}: expected the not-found refusal, got {body}"
        );
    }

    // A dry run over a request that is waiting: reported, and the request still waiting
    // afterwards.
    let id = waiting_request(&router, "the dry run", "10.40.0.0/16", "10.41.0.0/16").await;
    let waiting = described(&router, "the dry run", &id).await;
    let (status, body) = accept_in(&router, REGION, &id, &[param("DryRun", "true")]).await;
    assert_eq!(
        status, 412,
        "the dry run: expected HTTP 412, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "the dry run: expected the dry-run code, got {body}"
    );
    let after = described(&router, "after the dry run", &id).await;
    assert_eq!(
        after, waiting,
        "the dry run: a dry run must answer nothing, so the request is as it was: {waiting:?}"
    );
    let (stored, _message) = stored_status(&engine, &id);
    assert_eq!(
        stored, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
        "the dry run: the stored request must be left waiting"
    );
}
