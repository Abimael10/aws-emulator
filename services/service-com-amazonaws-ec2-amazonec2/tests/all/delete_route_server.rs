// SPDX-License-Identifier: CC0-1.0

//! `DeleteRouteServer`, driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! 1. the delete takes the route server the request NAMES down together with the resources OF
//!    it — its endpoints and their peers — putting each into its own non-terminal delete state
//!    at once and leaving the terminal one to the clock, and it answers the record the store
//!    then holds, so the delete and `DescribeRouteServers` cannot drift apart;
//! 2. a route server a VPC is still associated with is the thing writing that VPC's routes, so
//!    the delete is refused for the STATE it is in until the association has been removed, and
//!    the refusal names the VPC that holds it;
//! 3. every refusal is asked BEFORE `DryRun`: a request that would have been refused is
//!    refused with that reason, and one that would have succeeded is answered
//!    `DryRunOperation` and writes nothing.
//!
//! The rows differ along what the rules generalise over: how many endpoints hang off the route
//! server and how many peers off each endpoint (nothing at all, one endpoint, one endpoint with
//! two peers, two endpoints each with a peer), which state the association had reached when
//! the delete arrived, and which reason a request carrying `DryRun` would have been refused
//! for. A route server of a SECOND tree is built in every case of rule 1, so a cascade that
//! reached past the named route server would show up.
//!
//! The refusal sentences are spelled out here as literals rather than imported from
//! `ec2::state`: a suite that read the service's own constant would pass by agreeing with
//! itself.

use std::time::Duration;

use ec2::state::{
    DRY_RUN_OPERATION, INCORRECT_STATE, INVALID_ROUTE_SERVER_ID_MALFORMED,
    INVALID_ROUTE_SERVER_ID_NOT_FOUND, MISSING_PARAMETER, ROUTE_SERVER_ASSOCIATION_STATE_ASSOCIATED,
    ROUTE_SERVER_ASSOCIATION_STATE_ASSOCIATING, ROUTE_SERVER_ENDPOINT_STATE_DELETED,
    ROUTE_SERVER_ENDPOINT_STATE_DELETING, ROUTE_SERVER_PEER_STATE_DELETED,
    ROUTE_SERVER_PEER_STATE_DELETING, ROUTE_SERVER_STATE_DELETED, ROUTE_SERVER_STATE_DELETING,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed};
use super::wire::{param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The ASN the appliance's side of each BGP session runs under — inside the published range
/// `BgpOptions$PeerAsn` names ("valid values are from 1 to 4294967295").
const PEER_ASN: &str = "65001";

/// How long a poll for a clock-owed state may look before it gives up: generous, so a loaded
/// host cannot turn "not yet" into a failure, and never a bound on how fast the move has to
/// happen.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks at the same resource.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One kind of resource of the route server surface: the describe that reports it and the
/// request member that names one.
struct Kind {
    /// The operation that reports the state a resource of this kind has reached.
    describe: &'static str,
    /// The member a request names one by, which the describe takes as a list.
    id_member: &'static str,
}

/// The route server itself.
const ROUTE_SERVER: Kind = Kind {
    describe: "DescribeRouteServers",
    id_member: "RouteServerId",
};

/// An endpoint OF a route server.
const ENDPOINT: Kind = Kind {
    describe: "DescribeRouteServerEndpoints",
    id_member: "RouteServerEndpointId",
};

/// A BGP peer of an endpoint.
const PEER: Kind = Kind {
    describe: "DescribeRouteServerPeers",
    id_member: "RouteServerPeerId",
};

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
fn reported(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The state `kind`'s describe reports for the one resource `id` names.
async fn reported_state(case: &str, router: &MultiServiceRouter, kind: &Kind, id: &str) -> String {
    let body = ok(
        case,
        router,
        kind.describe,
        &indexed(kind.id_member, &[id]),
    )
    .await;
    reported(case, &body, "state")
}

/// Look at `id` again until its describe reports `wanted`, answering as soon as it does.
///
/// The clock, not a caller, moves a resource whose deletion is underway to its terminal state,
/// so this waits for the CONDITION rather than for a span: the deadline is far enough out that
/// a loaded machine is never the reason it fails, and nothing here asserts how fast the move
/// happened.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    kind: &Kind,
    id: &str,
    wanted: &str,
) {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    loop {
        let state = reported_state(case, router, kind, id).await;
        if state == wanted {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {id} to reach {wanted:?}, last read {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Pin that the resource `id` of `kind` is one a delete has taken down: in the non-terminal
/// `deleting` state the call put it in, or already in the terminal `deleted` the clock owes it.
///
/// Either state says the delete reached the resource; which of the two a read lands on is a
/// question of when the host got round to the read, never of whether the resource came down, so
/// nothing here turns on how fast the machine runs. The terminal state is waited for separately,
/// by polling the condition.
async fn in_a_delete_state(
    case: &str,
    router: &MultiServiceRouter,
    kind: &Kind,
    id: &str,
    deleting: &str,
    deleted: &str,
) {
    let state = reported_state(case, router, kind, id).await;
    assert!(
        state == deleting || state == deleted,
        "case {case:?}: expected {id} in a delete state ({deleting:?} or {deleted:?}) after the \
         delete of its route server, got {state:?}"
    );
}

/// One tree of route server resources: the route server, the endpoints of it and the peers of
/// those endpoints.
struct Surface {
    route_server_id: String,
    endpoint_ids: Vec<String>,
    peer_ids: Vec<String>,
}

/// Build such a tree over the wire: a VPC of its own, one subnet per endpoint, and
/// `peers_per_endpoint` BGP peers on each endpoint.
///
/// `second_octet` keeps each tree in its own address space (`10.<second_octet>.0.0/16`), so one
/// case's subnets and peer addresses can never be another's.
async fn surface(
    case: &str,
    router: &MultiServiceRouter,
    amazon_side_asn: &str,
    second_octet: u8,
    endpoints: usize,
    peers_per_endpoint: usize,
) -> Surface {
    let body = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", &format!("10.{second_octet}.0.0/16"))],
    )
    .await;
    let vpc_id = reported(case, &body, "vpcId");
    let body = ok(
        case,
        router,
        "CreateRouteServer",
        &[param("AmazonSideAsn", amazon_side_asn)],
    )
    .await;
    let route_server_id = reported(case, &body, "routeServerId");

    let mut endpoint_ids = Vec::with_capacity(endpoints);
    let mut peer_ids = Vec::with_capacity(endpoints * peers_per_endpoint);
    for index in 0..endpoints {
        let third_octet = index + 1;
        let body = ok(
            case,
            router,
            "CreateSubnet",
            &[
                param("VpcId", &vpc_id),
                param(
                    "CidrBlock",
                    &format!("10.{second_octet}.{third_octet}.0/24"),
                ),
            ],
        )
        .await;
        let subnet_id = reported(case, &body, "subnetId");
        let body = ok(
            case,
            router,
            "CreateRouteServerEndpoint",
            &[
                param("RouteServerId", &route_server_id),
                param("SubnetId", &subnet_id),
            ],
        )
        .await;
        let endpoint_id = reported(case, &body, "routeServerEndpointId");
        for peer in 0..peers_per_endpoint {
            let body = ok(
                case,
                router,
                "CreateRouteServerPeer",
                &[
                    param("RouteServerEndpointId", &endpoint_id),
                    param(
                        "PeerAddress",
                        &format!("10.{second_octet}.{third_octet}.{}", 20 + peer),
                    ),
                    param("BgpOptions.PeerAsn", PEER_ASN),
                ],
            )
            .await;
            peer_ids.push(reported(case, &body, "routeServerPeerId"));
        }
        endpoint_ids.push(endpoint_id);
    }
    Surface {
        route_server_id,
        endpoint_ids,
        peer_ids,
    }
}

/// RULE 1: the delete answers the route server it took down, in the non-terminal delete state,
/// with every member of the record still on it; the describe reports the same state straight
/// away; the endpoints and peers of that route server go down with it; and the clock — nothing
/// else — carries each of them to its terminal state. A second tree is untouched throughout.
#[tokio::test]
async fn delete_route_server_takes_down_the_route_server_and_the_resources_of_it() {
    // (how many endpoints hang off the route server, how many peers off each endpoint)
    let rows: [(usize, usize); 4] = [(0, 0), (1, 0), (1, 2), (2, 1)];

    for (endpoints, peers_per_endpoint) in rows {
        let case = format!("{endpoints} endpoint(s), {peers_per_endpoint} peer(s) on each");
        let case = case.as_str();
        let router = super::ec2_query::emulator().0;

        let amazon_side_asn = "64512";
        let target = surface(
            case,
            &router,
            amazon_side_asn,
            80,
            endpoints,
            peers_per_endpoint,
        )
        .await;
        // A tree nothing deletes, so a cascade that reached past the named route server would
        // show up here.
        let bystander = surface(case, &router, "64513", 90, 1, 1).await;

        let deleted = ok(
            case,
            &router,
            "DeleteRouteServer",
            &[param("RouteServerId", &target.route_server_id)],
        )
        .await;
        assert_eq!(
            reported(case, &deleted, "routeServerId"),
            target.route_server_id,
            "case {case:?}: expected the delete to answer the route server it took down, \
             got {deleted}"
        );
        assert_eq!(
            reported(case, &deleted, "state"),
            ROUTE_SERVER_STATE_DELETING,
            "case {case:?}: expected the delete to answer {ROUTE_SERVER_STATE_DELETING:?}, \
             got {deleted}"
        );
        // The answer is the stored record rather than a stub built for the delete, so the
        // members the create set are still on it.
        assert_eq!(
            reported(case, &deleted, "amazonSideAsn"),
            amazon_side_asn,
            "case {case:?}: expected the delete to answer amazonSideAsn {amazon_side_asn:?}, \
             got {deleted}"
        );

        // Straight after the call: the route server and every resource of it are records a
        // delete has taken down.
        in_a_delete_state(
            case,
            &router,
            &ROUTE_SERVER,
            &target.route_server_id,
            ROUTE_SERVER_STATE_DELETING,
            ROUTE_SERVER_STATE_DELETED,
        )
        .await;
        for endpoint_id in &target.endpoint_ids {
            in_a_delete_state(
                case,
                &router,
                &ENDPOINT,
                endpoint_id,
                ROUTE_SERVER_ENDPOINT_STATE_DELETING,
                ROUTE_SERVER_ENDPOINT_STATE_DELETED,
            )
            .await;
        }
        for peer_id in &target.peer_ids {
            in_a_delete_state(
                case,
                &router,
                &PEER,
                peer_id,
                ROUTE_SERVER_PEER_STATE_DELETING,
                ROUTE_SERVER_PEER_STATE_DELETED,
            )
            .await;
        }

        // The terminal states are the clock's to give, each resource on its own record.
        poll_until_state(
            case,
            &router,
            &ROUTE_SERVER,
            &target.route_server_id,
            ROUTE_SERVER_STATE_DELETED,
        )
        .await;
        for endpoint_id in &target.endpoint_ids {
            poll_until_state(
                case,
                &router,
                &ENDPOINT,
                endpoint_id,
                ROUTE_SERVER_ENDPOINT_STATE_DELETED,
            )
            .await;
        }
        for peer_id in &target.peer_ids {
            poll_until_state(
                case,
                &router,
                &PEER,
                peer_id,
                ROUTE_SERVER_PEER_STATE_DELETED,
            )
            .await;
        }

        // Nothing of the second tree is in a delete state: the cascade follows the records
        // that are resources OF the route server the request named, and nothing else.
        let untouched_states = [
            reported_state(case, &router, &ROUTE_SERVER, &bystander.route_server_id).await,
            reported_state(case, &router, &ENDPOINT, &bystander.endpoint_ids[0]).await,
            reported_state(case, &router, &PEER, &bystander.peer_ids[0]).await,
        ];
        for state in &untouched_states {
            assert!(
                state != ROUTE_SERVER_STATE_DELETING && state != ROUTE_SERVER_STATE_DELETED,
                "case {case:?}: expected the second tree out of the delete states, \
                 got {untouched_states:?}"
            );
        }
    }
}

/// RULE 2: a route server a VPC still holds an association with is refused for the state it is
/// in — in either state the association itself has reached — the refusal names that VPC, and
/// the same delete goes through once the association has been removed.
#[tokio::test]
async fn delete_route_server_is_refused_while_a_vpc_still_holds_an_association() {
    // Rows: the association read while it is still being established, and again once the clock
    // has established it — the dependency holds in either state.
    for settle_the_association in [false, true] {
        let case = if settle_the_association {
            "an established association"
        } else {
            "an association still being established"
        };
        let router = super::ec2_query::emulator().0;
        let body = ok(
            case,
            &router,
            "CreateVpc",
            &[param("CidrBlock", "10.70.0.0/16")],
        )
        .await;
        let vpc_id = reported(case, &body, "vpcId");
        let body = ok(
            case,
            &router,
            "CreateRouteServer",
            &[param("AmazonSideAsn", "64512")],
        )
        .await;
        let route_server_id = reported(case, &body, "routeServerId");
        let body = ok(
            case,
            &router,
            "AssociateRouteServer",
            &[
                param("RouteServerId", &route_server_id),
                param("VpcId", &vpc_id),
            ],
        )
        .await;
        let at_associate = reported(case, &body, "state");
        assert_eq!(
            at_associate, ROUTE_SERVER_ASSOCIATION_STATE_ASSOCIATING,
            "case {case:?}: expected the association reported \
             {ROUTE_SERVER_ASSOCIATION_STATE_ASSOCIATING:?} at once, got {at_associate:?}"
        );
        if settle_the_association {
            poll_until_association(
                case,
                &router,
                &route_server_id,
                ROUTE_SERVER_ASSOCIATION_STATE_ASSOCIATED,
            )
            .await;
        }

        // The sentence names the VPC that holds the route server, so the caller knows what to
        // disassociate; a dry run of the same request is answered with THAT reason rather than
        // with the probe's own.
        let expected = format!("Route Server has non-deleted association with VPC: {vpc_id}.");
        for probe in [false, true] {
            let mut params = vec![param("RouteServerId", &route_server_id)];
            if probe {
                params.push(param("DryRun", "true"));
            }
            let (status, body) = call(&router, "DeleteRouteServer", &params).await;
            assert_eq!(
                (status, error_code(&body)),
                (400, INCORRECT_STATE.to_string()),
                "case {case:?} (dry run {probe}): expected 400 {INCORRECT_STATE}, got {status} \
                 with body {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {case:?} (dry run {probe}): expected the message {expected:?}, \
                 got {message:?}"
            );
        }
        let held = reported_state(case, &router, &ROUTE_SERVER, &route_server_id).await;
        assert!(
            held != ROUTE_SERVER_STATE_DELETING && held != ROUTE_SERVER_STATE_DELETED,
            "case {case:?}: a refused delete must leave the route server alone, got {held:?}"
        );

        // The association removed, the delete the caller asked for is the one that happens.
        ok(
            case,
            &router,
            "DisassociateRouteServer",
            &[
                param("RouteServerId", &route_server_id),
                param("VpcId", &vpc_id),
            ],
        )
        .await;
        let deleted = ok(
            case,
            &router,
            "DeleteRouteServer",
            &[param("RouteServerId", &route_server_id)],
        )
        .await;
        assert_eq!(
            reported(case, &deleted, "state"),
            ROUTE_SERVER_STATE_DELETING,
            "case {case:?}: expected the delete of a disassociated route server to answer \
             {ROUTE_SERVER_STATE_DELETING:?}, got {deleted}"
        );
    }
}

/// Look again until `GetRouteServerAssociations` reports `wanted` for every association the
/// route server holds — the clock, not a caller, establishes one.
async fn poll_until_association(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
    wanted: &str,
) {
    let deadline = tokio::time::Instant::now() + POLL_DEADLINE;
    loop {
        let body = ok(
            case,
            router,
            "GetRouteServerAssociations",
            &[param("RouteServerId", route_server_id)],
        )
        .await;
        let states = elements(&body, "state");
        if !states.is_empty() && states.iter().all(|state| state == wanted) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected every association of {route_server_id} to reach \
             {wanted:?}, last read {states:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE 3: a request that would have succeeded is answered `DryRunOperation` and writes
/// nothing — the delete it probed still goes through afterwards — and a request that would have
/// been refused is refused with that reason instead, whichever reason it is.
#[tokio::test]
async fn a_dry_run_is_answered_without_a_write_and_every_refusal_wins_over_it() {
    let case = "a route server nothing holds";
    let router = super::ec2_query::emulator().0;
    let body = ok(
        case,
        &router,
        "CreateRouteServer",
        &[param("AmazonSideAsn", "64512")],
    )
    .await;
    let live = reported(case, &body, "routeServerId");

    let (status, body) = call(
        &router,
        "DeleteRouteServer",
        &[param("RouteServerId", &live), param("DryRun", "true")],
    )
    .await;
    assert_eq!(
        (status, error_code(&body)),
        (412, DRY_RUN_OPERATION.to_string()),
        "case {case:?}: expected 412 {DRY_RUN_OPERATION}, got {status} with body {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, "Request would have succeeded, but DryRun flag is set.",
        "case {case:?}: expected the probe's own sentence, got {message:?}"
    );
    let after_the_probe = reported_state(case, &router, &ROUTE_SERVER, &live).await;
    assert!(
        after_the_probe != ROUTE_SERVER_STATE_DELETING
            && after_the_probe != ROUTE_SERVER_STATE_DELETED,
        "case {case:?}: the probe must write nothing, got {after_the_probe:?}"
    );

    // A route server whose delete is underway, for the row that probes one.
    let body = ok(
        case,
        &router,
        "CreateRouteServer",
        &[param("AmazonSideAsn", "64513")],
    )
    .await;
    let going_down = reported(case, &body, "routeServerId");
    ok(
        case,
        &router,
        "DeleteRouteServer",
        &[param("RouteServerId", &going_down)],
    )
    .await;

    // (the case, the id the request names — `None` for a request that names none, the code the
    // reason it would have been refused for carries)
    let rows: Vec<(&str, Option<String>, &str)> = vec![
        ("no routeServerId at all", None, MISSING_PARAMETER),
        (
            "a routeServerId carried with no value",
            Some(String::new()),
            MISSING_PARAMETER,
        ),
        (
            "a long-form id the account holds no route server for",
            Some("rs-f0123456789abcdef".to_string()),
            INVALID_ROUTE_SERVER_ID_NOT_FOUND,
        ),
        (
            "a short-form id the account holds no route server for",
            Some("rs-f0123456".to_string()),
            INVALID_ROUTE_SERVER_ID_NOT_FOUND,
        ),
        (
            "something that is not a route server id at all",
            Some("vpc-0123456789abcdef0".to_string()),
            INVALID_ROUTE_SERVER_ID_MALFORMED,
        ),
        (
            "a route server whose delete is already underway",
            Some(going_down.clone()),
            INCORRECT_STATE,
        ),
    ];
    for (row, id, code) in rows {
        let mut params = vec![param("DryRun", "true")];
        if let Some(id) = &id {
            params.push(param("RouteServerId", id));
        }
        let (status, body) = call(&router, "DeleteRouteServer", &params).await;
        assert_eq!(
            (status, error_code(&body)),
            (400, code.to_string()),
            "case {row:?}: expected 400 {code} rather than the dry run's answer, got {status} \
             with body {body}"
        );
    }
    // A request that carried the parameter but no value names no route server to delete, and
    // the handler names the parameter it needed in EC2's own sentence for that class.
    let (_, body) = call(
        &router,
        "DeleteRouteServer",
        &[param("RouteServerId", ""), param("DryRun", "true")],
    )
    .await;
    let message = error_message(&body);
    assert_eq!(
        message, "The request must contain the parameter routeServerId",
        "case \"a routeServerId carried with no value\": expected EC2's missing-parameter \
         sentence, got {message:?}"
    );

    // The probe consumed nothing: the delete it asked about is still there to be made.
    let deleted = ok(
        case,
        &router,
        "DeleteRouteServer",
        &[param("RouteServerId", &live)],
    )
    .await;
    assert_eq!(
        reported(case, &deleted, "state"),
        ROUTE_SERVER_STATE_DELETING,
        "case {case:?}: expected the probed delete to go through afterwards, got {deleted}"
    );
}
