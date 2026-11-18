// SPDX-License-Identifier: CC0-1.0

//! `EnableRouteServerPropagation` and `DisableRouteServerPropagation` write ONE PAIR.
//!
//! A route server propagation is the (route server, route table) pair itself — the modelled
//! `RouteServerPropagation` shape carries no id of its own
//! (`smithy_query(query="shape",shape="RouteServerPropagation")`), only `RouteServerId`,
//! `RouteTableId` and `State` — so each pair is an independent record of the store.
//!
//! The rule under test: an enable records exactly the pair the request names and a disable
//! takes back exactly the pair the request names, so every OTHER pair — another table of the
//! same route server, and a table of another route server in another VPC — keeps the
//! propagation and the state it already had, and a pair whose disable has been carried out
//! can be enabled afresh without disturbing the pairs around it.
//!
//! That is the write side of the slice `route_server_propagations.rs` covers from the read
//! side: the suite beside this one drives one propagated table, so the independence of the
//! pairs of ONE route server, and of the pairs of two route servers, is what this one pins.
//!
//! The rows differ in WHICH of a route server's pairs the disable names (its first, the one
//! in the middle, its last), with the route server holding three pairs in one VPC and a
//! second route server holding two more in another VPC throughout — so no one pair, and no
//! one position in the set, is the specification.
//!
//! No step waits a fixed time for a clock-owed move: each is looked for AGAIN until the
//! service reports it, or a deadline far past the service's own transition span passes, so
//! nothing here depends on how fast the host runs.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2::state::{
    ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE, ROUTE_SERVER_PROPAGATION_STATE_DELETING,
    ROUTE_SERVER_PROPAGATION_STATE_PENDING, TRANSITION_DELAY,
};
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The deadline every poll below gives up at: a large multiple of the service's own
/// transition span, so it only ever bounds a hang rather than timing the service.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 120
}

/// One look inside that deadline, short enough that the deadline costs many looks rather
/// than one.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn action(router: &MultiServiceRouter, name: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, name, params).await
}

/// One call that must succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, name: &str, params: &[Param]) -> String {
    let (status, body) = action(router, name, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {name}, got {status} with body {body}"
    );
    body
}

/// A route server with `amazon_side_asn`, and the id it was minted with.
async fn create_route_server(
    case: &str,
    router: &MultiServiceRouter,
    amazon_side_asn: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateRouteServer",
        &[param("AmazonSideAsn", amazon_side_asn)],
    )
    .await;
    element(&body, "routeServerId")
        .unwrap_or_else(|| panic!("case {case:?}: no routeServerId in {body}"))
}

/// A VPC with `cidr_block`, and the id it was minted with.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId in {body}"))
}

/// A route table of the VPC `vpc_id`, and the id it was minted with.
async fn create_route_table(case: &str, router: &MultiServiceRouter, vpc_id: &str) -> String {
    let body = ok(case, router, "CreateRouteTable", &[param("VpcId", vpc_id)]).await;
    element(&body, "routeTableId")
        .unwrap_or_else(|| panic!("case {case:?}: no routeTableId in {body}"))
}

/// `AssociateRouteServer` for one (route server, VPC) pair, which must succeed: propagation
/// installs its routes through that association.
async fn associate(case: &str, router: &MultiServiceRouter, route_server_id: &str, vpc_id: &str) {
    ok(
        case,
        router,
        "AssociateRouteServer",
        &[
            param("RouteServerId", route_server_id),
            param("VpcId", vpc_id),
        ],
    )
    .await;
}

/// The form fields the enable and the disable share.
fn pair_params(route_server_id: &str, route_table_id: &str) -> Vec<Param> {
    vec![
        param("RouteServerId", route_server_id),
        param("RouteTableId", route_table_id),
    ]
}

/// Every propagation a response carries, as `(routeServerId, routeTableId, state)` triples:
/// the element that holds BOTH ids, which is the one shape the enable, the disable and the
/// read all answer a propagation in.
fn propagations(case: &str, body: &str) -> Vec<(String, String, String)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "routeServerId").is_some()
                && child(*node, "routeTableId").is_some()
        })
        .map(|node| {
            (
                child_text(node, "routeServerId").unwrap_or_default(),
                child_text(node, "routeTableId").unwrap_or_default(),
                child_text(node, "state").unwrap_or_default(),
            )
        })
        .collect()
}

/// The `routeTableId` -> `state` map `GetRouteServerPropagations` reports for
/// `route_server_id`, each propagation having first been asserted to name that route server.
async fn reported(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
) -> BTreeMap<String, String> {
    let body = ok(
        case,
        router,
        "GetRouteServerPropagations",
        &[param("RouteServerId", route_server_id)],
    )
    .await;
    propagations(case, &body)
        .into_iter()
        .map(|(reported_server, reported_table, state)| {
            assert_eq!(
                reported_server, route_server_id,
                "case {case:?}: a propagation of {route_server_id} must name that route \
                 server, got {reported_server:?}"
            );
            (reported_table, state)
        })
        .collect()
}

/// `EnableRouteServerPropagation` for one pair, which must answer the pair it was asked for
/// in the non-terminal state a propagation is recorded in.
async fn enable(case: &str, router: &MultiServiceRouter, route_server_id: &str, route_table_id: &str) {
    let body = ok(
        case,
        router,
        "EnableRouteServerPropagation",
        &pair_params(route_server_id, route_table_id),
    )
    .await;
    assert_eq!(
        propagations(case, &body),
        vec![(
            route_server_id.to_string(),
            route_table_id.to_string(),
            ROUTE_SERVER_PROPAGATION_STATE_PENDING.to_string()
        )],
        "case {case:?}: the enable must answer the pair it was asked for in \
         {ROUTE_SERVER_PROPAGATION_STATE_PENDING}, got body {body}"
    );
}

/// `DisableRouteServerPropagation` for one pair, which must answer that pair alone, being
/// taken down.
async fn disable(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
    route_table_id: &str,
) {
    let body = ok(
        case,
        router,
        "DisableRouteServerPropagation",
        &pair_params(route_server_id, route_table_id),
    )
    .await;
    assert_eq!(
        propagations(case, &body),
        vec![(
            route_server_id.to_string(),
            route_table_id.to_string(),
            ROUTE_SERVER_PROPAGATION_STATE_DELETING.to_string()
        )],
        "case {case:?}: the disable must answer the pair it was asked for in \
         {ROUTE_SERVER_PROPAGATION_STATE_DELETING}, got body {body}"
    );
}

/// Look at `route_server_id`'s propagations AGAIN until the tables it reports are exactly
/// `wanted`, or the deadline passes. Answers the map it then reported, so the caller asserts
/// on what the service really said.
async fn poll_until_tables(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
    wanted: &[String],
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let held = reported(case, router, route_server_id).await;
        let tables: Vec<String> = held.keys().cloned().collect();
        if tables == wanted {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {route_server_id} must report the tables {wanted:?}, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Look at `route_server_id`'s propagations AGAIN until every one of them is in force, or the
/// deadline passes. Answers the map it then reported.
async fn poll_until_all_available(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
    wanted: &[String],
) -> BTreeMap<String, String> {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let held = poll_until_tables(case, router, route_server_id, wanted).await;
        if held
            .values()
            .all(|state| state == ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE)
        {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every propagation of {route_server_id} must reach \
             {ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE}, got {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The map the tables `tables` are expected to be reported as, every one of them in force.
fn all_available(tables: &[String]) -> BTreeMap<String, String> {
    tables
        .iter()
        .map(|table| {
            (
                table.clone(),
                ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE.to_string(),
            )
        })
        .collect()
}

/// The rule: each (route server, route table) pair is its own propagation, so an enable
/// records exactly the pair the request names and a disable takes back exactly the pair the
/// request names — every other pair of that route server, and every pair of another route
/// server, keeps the propagation and the state it already had.
///
/// The rows differ in which of the route server's three pairs the disable names: its first,
/// the one in the middle, and its last.
#[tokio::test]
async fn a_propagation_write_touches_only_the_pair_the_request_names() {
    // One row per position of the pair the disable names, each driven against its own
    // emulator so no row inherits another's store.
    for position in 0_usize..3 {
        let case = format!("disabling the pair at position {position} of three");
        let case = case.as_str();
        let router = super::ec2_query::emulator().0;

        // The route server under test, holding three pairs in its own VPC, and a second one
        // holding two more in another VPC: a propagation is a pair, so the second route
        // server's pairs must be untouched by every write aimed at the first's.
        let server = create_route_server(case, &router, "64512").await;
        let other_server = create_route_server(case, &router, "4200000000").await;
        let vpc = create_vpc(case, &router, "10.61.0.0/16").await;
        let other_vpc = create_vpc(case, &router, "10.62.0.0/16").await;
        associate(case, &router, &server, &vpc).await;
        associate(case, &router, &other_server, &other_vpc).await;

        let mut tables = Vec::new();
        for _ in 0..3 {
            tables.push(create_route_table(case, &router, &vpc).await);
        }
        let mut other_tables = Vec::new();
        for _ in 0..2 {
            other_tables.push(create_route_table(case, &router, &other_vpc).await);
        }
        // The store keeps propagations in RouteTableId order and this map is read in that
        // same order, so the expectations below are the ids sorted.
        tables.sort();
        other_tables.sort();

        // Every pair is enabled, the two route servers' enables interleaved, and after each
        // one the TABLES each route server propagates into are exactly the pairs enabled so
        // far: an enable adds its own pair and nothing else.
        let mut expected: Vec<String> = Vec::new();
        let mut other_expected: Vec<String> = Vec::new();
        for (index, table) in tables.iter().enumerate() {
            enable(case, &router, &server, table).await;
            expected.push(table.clone());
            expected.sort();
            if let Some(other_table) = other_tables.get(index) {
                enable(case, &router, &other_server, other_table).await;
                other_expected.push(other_table.clone());
                other_expected.sort();
            }
            let held = reported(case, &router, &server).await;
            assert_eq!(
                held.keys().cloned().collect::<Vec<String>>(),
                expected,
                "case {case:?}: after enabling {expected:?}, {server} must propagate into \
                 exactly those tables, got {held:?}"
            );
            let other_held = reported(case, &router, &other_server).await;
            assert_eq!(
                other_held.keys().cloned().collect::<Vec<String>>(),
                other_expected,
                "case {case:?}: enabling a pair of {server} must leave {other_server} \
                 propagating into exactly {other_expected:?}, got {other_held:?}"
            );
        }

        // Once the clock has carried every enable out, both route servers hold their pairs in
        // force — the state each pair the disable below does NOT name must still be in
        // afterwards.
        let settled = poll_until_all_available(case, &router, &server, &expected).await;
        assert_eq!(
            settled,
            all_available(&expected),
            "case {case:?}: every pair of {server} must be in force, got {settled:?}"
        );
        let other_settled =
            poll_until_all_available(case, &router, &other_server, &other_expected).await;
        assert_eq!(
            other_settled,
            all_available(&other_expected),
            "case {case:?}: every pair of {other_server} must be in force, got \
             {other_settled:?}"
        );

        // The disable of ONE pair: it answers that pair being taken down...
        let disabled = tables[position].clone();
        disable(case, &router, &server, &disabled).await;

        // ...and once the clock has carried it out, that pair alone has stopped propagating.
        // The pairs the request did not name are still in force, and so are both pairs of the
        // other route server.
        let remaining: Vec<String> = expected
            .iter()
            .filter(|table| **table != disabled)
            .cloned()
            .collect();
        let after = poll_until_tables(case, &router, &server, &remaining).await;
        assert_eq!(
            after,
            all_available(&remaining),
            "case {case:?}: disabling {disabled} must leave the pairs {remaining:?} in \
             force, got {after:?}"
        );
        let other_after = reported(case, &router, &other_server).await;
        assert_eq!(
            other_after,
            all_available(&other_expected),
            "case {case:?}: disabling a pair of {server} must leave {other_server}'s pairs \
             in force, got {other_after:?}"
        );

        // The pair whose disable was carried out can be enabled afresh, and that enable too
        // touches one pair: the pairs around it keep the state they were in, and the other
        // route server keeps its own.
        enable(case, &router, &server, &disabled).await;
        let re_enabled = reported(case, &router, &server).await;
        assert_eq!(
            re_enabled.keys().cloned().collect::<Vec<String>>(),
            expected,
            "case {case:?}: re-enabling {disabled} must restore the pair, got {re_enabled:?}"
        );
        for table in &remaining {
            assert_eq!(
                re_enabled.get(table).map(String::as_str),
                Some(ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE),
                "case {case:?}: re-enabling {disabled} must leave {table} in force, got \
                 {re_enabled:?}"
            );
        }
        let re_enabled_state = re_enabled
            .get(&disabled)
            .map(String::as_str)
            .unwrap_or_default();
        assert!(
            re_enabled_state == ROUTE_SERVER_PROPAGATION_STATE_PENDING
                || re_enabled_state == ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE,
            "case {case:?}: the re-enabled pair {disabled} must be \
             {ROUTE_SERVER_PROPAGATION_STATE_PENDING} or \
             {ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE}, got {re_enabled_state:?}"
        );
        let other_re_enabled = reported(case, &router, &other_server).await;
        assert_eq!(
            other_re_enabled,
            all_available(&other_expected),
            "case {case:?}: re-enabling a pair of {server} must leave {other_server}'s \
             pairs in force, got {other_re_enabled:?}"
        );
    }
}
