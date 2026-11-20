// SPDX-License-Identifier: CC0-1.0

//! What `GetRouteServerRoutingDatabase` reports for `AreRoutesPersisted` when the route
//! server was CONFIGURED to persist its routes — driven over the wire the service actually
//! speaks (`ec2Query`, through the crate's shared [`super::ec2_query`] harness).
//!
//! The rule under test: "Indicates whether routes are being persisted in the routing
//! database" (`GetRouteServerRoutingDatabaseResult$AreRoutesPersisted`) is a fact of the
//! DATABASE and not of the configuration — routes are being persisted only when the database
//! holds a learned route to persist and the route server's persistence has settled on
//! `enabled` — so a route server whose peers have established no BGP session reports `false`
//! with an empty set of routes, whatever `PersistRoutes` it was created or modified with,
//! while `DescribeRouteServers` goes on reporting that configuration. The member therefore
//! tells a caller "configured to persist" from "actually persisting learned routes" instead of
//! echoing the request parameter.
//!
//! Each row is a distinct input along what the rule generalises over — how many peers the
//! route server holds and which liveness protocol each uses, the persistence the route server
//! was configured with and at which published duration — so no one row is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements};
use super::rbin_retention_rules::poll_until;

/// The Region every call here is signed for: the default VPC's subnets — the subnets a route
/// server endpoint is placed into — are seeded for the emulator's default Region.
const REGION: &str = "us-east-1";

/// One request's form parameters.
type Params = Vec<(String, String)>;

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call that must succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, name: &str, params: &Params) -> String {
    let (status, body) = call(router, REGION, name, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {name}, got {status} with body {body}"
    );
    body
}

/// A route server with `extra` beside its ASN, and the id it was minted with.
async fn create_route_server(case: &str, router: &MultiServiceRouter, asn: i64, extra: Params) -> String {
    let mut params = vec![p("AmazonSideAsn", asn.to_string())];
    params.extend(extra);
    let body = ok(case, router, "CreateRouteServer", &params).await;
    element(&body, "routeServerId")
        .unwrap_or_else(|| panic!("case {case:?}: no routeServerId in {body}"))
}

/// An endpoint of `route_server_id` in the first subnet the account holds, and its id.
async fn create_endpoint(case: &str, router: &MultiServiceRouter, route_server_id: &str) -> String {
    let subnets = ok(case, router, "DescribeSubnets", &Vec::new()).await;
    let subnet_id = elements(&subnets, "subnetId")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the account holds no subnet"));
    let body = ok(
        case,
        router,
        "CreateRouteServerEndpoint",
        &vec![
            p("RouteServerId", route_server_id),
            p("SubnetId", subnet_id),
        ],
    )
    .await;
    element(&body, "routeServerEndpointId")
        .unwrap_or_else(|| panic!("case {case:?}: no routeServerEndpointId in {body}"))
}

/// A peer of `endpoint_id`, with the liveness protocol the row names.
async fn create_peer(
    case: &str,
    router: &MultiServiceRouter,
    endpoint_id: &str,
    address: &str,
    peer_asn: i64,
    liveness: Option<&str>,
) {
    let mut params = vec![
        p("RouteServerEndpointId", endpoint_id),
        p("PeerAddress", address),
        p("BgpOptions.PeerAsn", peer_asn.to_string()),
    ];
    if let Some(liveness) = liveness {
        params.push(p("BgpOptions.PeerLivenessDetection", liveness));
    }
    ok(case, router, "CreateRouteServerPeer", &params).await;
}

/// The `persistRoutesState` `DescribeRouteServers` reports for `route_server_id`.
async fn described_persistence(case: &str, router: &MultiServiceRouter, route_server_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeRouteServers",
        &vec![p("RouteServerId.1", route_server_id)],
    )
    .await;
    element(&body, "persistRoutesState")
        .unwrap_or_else(|| panic!("case {case:?}: no persistRoutesState in {body}"))
}

/// Look again until the route server's persistence configuration reports `wanted`, waiting for
/// that condition rather than for the service's own transition span.
async fn poll_until_persistence(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
    wanted: &str,
) {
    poll_until(case, "the route server's persistence to settle", || async move {
        described_persistence(case, router, route_server_id).await == wanted
    })
    .await;
}

/// One row: the case, the `PersistRoutes` members the create carries, the settled
/// `persistRoutesState` they reach, and the peers to place on one endpoint (the liveness
/// protocol each is created with, `None` for a create that names none).
type Row = (
    &'static str,
    Params,
    &'static str,
    Vec<Option<&'static str>>,
);

/// The rule: a database with no learned route persists nothing, whatever the route server was
/// configured with — and the configuration stays readable where it is reported.
#[tokio::test]
async fn a_database_with_no_learned_route_persists_nothing() {
    let (floor, ceiling) = ec2::state::PERSIST_ROUTES_DURATION_RANGE;
    let enabled_at = |duration: i64| -> Params {
        vec![
            p("PersistRoutes", "enable"),
            p("PersistRoutesDuration", duration.to_string()),
        ]
    };
    let enabled = ec2::state::PERSIST_ROUTES_STATE_ENABLED;
    let disabled = ec2::state::PERSIST_ROUTES_STATE_DISABLED;

    let rows: Vec<Row> = vec![
        (
            "persistence asked for at the published floor, with no peer at all",
            enabled_at(floor),
            enabled,
            Vec::new(),
        ),
        (
            "persistence asked for at the published floor, with one BFD peer",
            enabled_at(floor),
            enabled,
            vec![Some("bfd")],
        ),
        (
            "persistence asked for at the published ceiling, with two peers",
            enabled_at(ceiling),
            enabled,
            vec![Some("bgp-keepalive"), None],
        ),
        (
            "no persistence asked for, with one peer",
            Vec::new(),
            disabled,
            vec![Some("bgp-keepalive")],
        ),
    ];

    for (case, created_with, settles_on, peers) in rows {
        let router = emulator();
        let route_server_id = create_route_server(case, &router, 64_512, created_with).await;
        if !peers.is_empty() {
            let endpoint_id = create_endpoint(case, &router, &route_server_id).await;
            for (index, liveness) in peers.iter().enumerate() {
                let address = format!("198.51.100.{}", index + 40);
                create_peer(
                    case,
                    &router,
                    &endpoint_id,
                    &address,
                    64_700 + index as i64,
                    *liveness,
                )
                .await;
            }
        }

        // The route server reaches the configuration the row asked for: the CLOCK settles it,
        // and this waits for that word rather than for a span.
        poll_until_persistence(case, &router, &route_server_id, settles_on).await;

        // The PREMISE the database is derived from: nothing in this emulator speaks BGP to a
        // peer address, so no session is established and no route is learned. A session
        // reported `up` would have to carry the routes it learned, and the assertion below
        // would then be wrong rather than merely unmet.
        let peers_body = ok(case, &router, "DescribeRouteServerPeers", &Vec::new()).await;
        let statuses = elements(&peers_body, "status");
        assert!(
            statuses.iter().all(|status| status != "up"),
            "case {case:?}: no session of this emulator is established, got {statuses:?}"
        );

        let body = ok(
            case,
            &router,
            "GetRouteServerRoutingDatabase",
            &vec![p("RouteServerId", route_server_id.clone())],
        )
        .await;
        let prefixes = elements(&body, "prefix");
        assert!(
            prefixes.is_empty(),
            "case {case:?}: a database no session carried a route into must hold no entry, \
             got {prefixes:?} in {body}"
        );
        let persisted = element(&body, "areRoutesPersisted");
        assert_eq!(
            persisted.as_deref(),
            Some("false"),
            "case {case:?}: there is no learned route to persist, so areRoutesPersisted must \
             be false, got {persisted:?} in {body}"
        );

        // ...while the CONFIGURATION is still what the read that reports it says: the
        // database member is no echo of it.
        let described = described_persistence(case, &router, &route_server_id).await;
        assert_eq!(
            described, settles_on,
            "case {case:?}: expected persistRoutesState {settles_on:?} to be reported \
             unchanged, got {described:?}"
        );
    }
}
