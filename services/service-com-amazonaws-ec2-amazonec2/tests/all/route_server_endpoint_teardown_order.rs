// SPDX-License-Identifier: CC0-1.0

//! The teardown ORDER of the route server surface, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: **a route server endpoint cannot be deleted while a peer of it has not
//! been deleted.** A peer is "a BGP peer configured to exchange routes with a route server
//! endpoint" (`CreateRouteServerPeer`), so it is reached THROUGH the endpoint and cannot
//! outlive it: EC2 refuses the delete `IncorrectState`, naming the peer that still holds the
//! endpoint, and leaves the endpoint as it was. The same delete goes through once the peers
//! have been taken down.
//!
//! The rows differ along what the rule generalises over: how many peers the endpoint carries,
//! and whether a peer is still coming up or has reached `available` — the dependency holds in
//! either state. The endpoint is read back through `DescribeRouteServerEndpoints` after each
//! refusal, so a refused delete is shown to have changed nothing.

use ec2::state::{
    INCORRECT_STATE, ROUTE_SERVER_ENDPOINT_STATE_DELETING, ROUTE_SERVER_PEER_STATE_AVAILABLE,
    ROUTE_SERVER_PEER_STATE_DELETING, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The Amazon-side ASN the route server under test is created with — inside the published
/// range `CreateRouteServerRequest$AmazonSideAsn` names ("valid values are from 1 to
/// 4294967295").
const AMAZON_SIDE_ASN: &str = "64512";

/// The ASN the appliance's side of each BGP session runs under.
const PEER_ASN: &str = "65001";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The route server surface one case tears down: the route server, the VPC it serves, the
/// endpoint in a subnet of that VPC, and the peers of that endpoint.
struct Surface {
    endpoint_id: String,
    peer_ids: Vec<String>,
}

/// Build that surface over the wire, with `peers` BGP peers on the endpoint.
async fn surface(case: &str, router: &MultiServiceRouter, peers: usize) -> Surface {
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", "10.80.0.0/16")]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", "10.80.1.0/24")],
    )
    .await;
    let subnet_id = member(case, &subnet, "subnetId");
    let route_server = ok(
        case,
        router,
        "CreateRouteServer",
        &[param("AmazonSideAsn", AMAZON_SIDE_ASN)],
    )
    .await;
    let route_server_id = member(case, &route_server, "routeServerId");
    ok(
        case,
        router,
        "AssociateRouteServer",
        &[
            param("RouteServerId", &route_server_id),
            param("VpcId", &vpc_id),
        ],
    )
    .await;
    let endpoint = ok(
        case,
        router,
        "CreateRouteServerEndpoint",
        &[
            param("RouteServerId", &route_server_id),
            param("SubnetId", &subnet_id),
        ],
    )
    .await;
    let endpoint_id = member(case, &endpoint, "routeServerEndpointId");
    let mut peer_ids = Vec::with_capacity(peers);
    for index in 0..peers {
        let peer = ok(
            case,
            router,
            "CreateRouteServerPeer",
            &[
                param("RouteServerEndpointId", &endpoint_id),
                param("PeerAddress", &format!("10.80.1.{}", 10 + index)),
                param("BgpOptions.PeerAsn", PEER_ASN),
            ],
        )
        .await;
        peer_ids.push(member(case, &peer, "routeServerPeerId"));
    }
    Surface {
        endpoint_id,
        peer_ids,
    }
}

/// The `State` the endpoint `endpoint_id` is reported in, read through the XML tree so a member
/// of the endpoint is never confused with one of the same name elsewhere in the envelope.
async fn endpoint_state(case: &str, router: &MultiServiceRouter, endpoint_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeRouteServerEndpoints",
        &[param("RouteServerEndpointId.1", endpoint_id)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "routeServerEndpointId").as_deref() == Some(endpoint_id)
        })
        .and_then(|node| child_text(node, "state"))
        .unwrap_or_else(|| panic!("case {case:?}: no state for {endpoint_id} in {body}"))
}

/// Look again until every peer of `peer_ids` reports `available`, or a generous deadline
/// passes.
///
/// The condition is looked at AGAIN in a loop rather than waited out, so nothing here depends
/// on how fast the host runs; the deadline is a multiple of the service's own transition span,
/// far enough out that a loaded machine is never the reason it fails.
async fn poll_until_peers_are_available(
    case: &str,
    router: &MultiServiceRouter,
    peer_ids: &[String],
) {
    let deadline = std::time::Instant::now() + TRANSITION_DELAY * 40;
    loop {
        let body = ok(case, router, "DescribeRouteServerPeers", &[]).await;
        let states = elements(&body, "state");
        let available = states
            .iter()
            .filter(|state| *state == ROUTE_SERVER_PEER_STATE_AVAILABLE)
            .count();
        if available >= peer_ids.len() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: every peer of {peer_ids:?} must reach \
             {ROUTE_SERVER_PEER_STATE_AVAILABLE:?}, last read {states:?}"
        );
        tokio::time::sleep(TRANSITION_DELAY / 10).await;
    }
}

/// The rule: the delete is refused while a peer of the endpoint has not been deleted, in either
/// state the peer takes, and goes through once the peers are down.
#[tokio::test]
async fn an_endpoint_is_not_deleted_while_a_peer_still_holds_it() {
    // Rows: one peer and two, each read while the peers are still coming up and again once the
    // clock has brought them up.
    for (peers, wait_for_the_peers) in [(1usize, false), (1, true), (2, false), (2, true)] {
        let case = format!(
            "{peers} peer(s), {}",
            if wait_for_the_peers {
                "established"
            } else {
                "still coming up"
            }
        );
        let case = case.as_str();
        let router = super::ec2_query::emulator().0;
        let surface = surface(case, &router, peers).await;
        if wait_for_the_peers {
            poll_until_peers_are_available(case, &router, &surface.peer_ids).await;
        }

        let (status, refused) = call(
            &router,
            "DeleteRouteServerEndpoint",
            &[param("RouteServerEndpointId", &surface.endpoint_id)],
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, INCORRECT_STATE),
            "case {case:?}: expected 400 {INCORRECT_STATE} while a peer holds the endpoint, got \
             {status} with body {refused}"
        );
        let message = error_message(&refused);
        assert!(
            surface
                .peer_ids
                .iter()
                .any(|peer_id| message.contains(peer_id)),
            "case {case:?}: expected the refusal to name a peer of {peer_ids:?}, got {message}",
            peer_ids = surface.peer_ids
        );
        let held = endpoint_state(case, &router, &surface.endpoint_id).await;
        assert!(
            held != ROUTE_SERVER_ENDPOINT_STATE_DELETING,
            "case {case:?}: the refused delete must not take the endpoint down, got {held:?}"
        );

        // The peers taken down first, the same delete is the one the caller asked for: a peer
        // already going down holds the endpoint no longer.
        for peer_id in &surface.peer_ids {
            let deleted = ok(
                case,
                &router,
                "DeleteRouteServerPeer",
                &[param("RouteServerPeerId", peer_id)],
            )
            .await;
            assert_eq!(
                element(&deleted, "state").as_deref(),
                Some(ROUTE_SERVER_PEER_STATE_DELETING),
                "case {case:?}: expected the peer {peer_id} reported going down, got {deleted}"
            );
        }
        let deleted = ok(
            case,
            &router,
            "DeleteRouteServerEndpoint",
            &[param("RouteServerEndpointId", &surface.endpoint_id)],
        )
        .await;
        assert_eq!(
            element(&deleted, "state").as_deref(),
            Some(ROUTE_SERVER_ENDPOINT_STATE_DELETING),
            "case {case:?}: expected the endpoint reported going down, got {deleted}"
        );
        assert_eq!(
            endpoint_state(case, &router, &surface.endpoint_id).await,
            ROUTE_SERVER_ENDPOINT_STATE_DELETING,
            "case {case:?}: the describe reports the endpoint the delete left"
        );
    }
}

/// An endpoint carrying NO peer is deleted straight away: the dependency is the peer, not the
/// endpoint's own age — and a refusal is not consumed by being read, so an endpoint a peer
/// holds refuses every delete until the peer is gone.
#[tokio::test]
async fn an_endpoint_with_no_peer_is_deleted_and_one_with_a_peer_keeps_refusing() {
    let case = "an endpoint carrying no peer";
    let router = super::ec2_query::emulator().0;
    let free = surface(case, &router, 0).await;
    let deleted = ok(
        case,
        &router,
        "DeleteRouteServerEndpoint",
        &[param("RouteServerEndpointId", &free.endpoint_id)],
    )
    .await;
    assert_eq!(
        element(&deleted, "state").as_deref(),
        Some(ROUTE_SERVER_ENDPOINT_STATE_DELETING),
        "case {case:?}: an endpoint nothing holds comes down, got {deleted}"
    );

    let case = "an endpoint a peer holds";
    let held = surface(case, &router, 1).await;
    for attempt in 1..=3 {
        let (status, refused) = call(
            &router,
            "DeleteRouteServerEndpoint",
            &[param("RouteServerEndpointId", &held.endpoint_id)],
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, INCORRECT_STATE),
            "attempt {attempt}: expected 400 {INCORRECT_STATE}, got {status} with body {refused}"
        );
        assert!(
            endpoint_state(case, &router, &held.endpoint_id).await
                != ROUTE_SERVER_ENDPOINT_STATE_DELETING,
            "attempt {attempt}: the endpoint must still be one the delete has not taken down"
        );
    }
}
