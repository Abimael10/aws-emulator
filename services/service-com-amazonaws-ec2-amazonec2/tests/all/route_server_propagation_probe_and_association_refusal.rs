// SPDX-License-Identifier: CC0-1.0

//! What the route server PROPAGATION writes answer when the route server holds no
//! association, and where a `DryRun` of one of them is answered — driven over the wire the
//! service actually speaks (`ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness).
//!
//! The rules under test:
//!
//! 1. the refusal of a propagation whose route server is not associated names THAT ROUTE
//!    SERVER and the association it lacks — `InvalidRouteServerId.NotAssociated`, "Route
//!    Server <id> is not associated with a VPC." — and nothing else: propagation installs the
//!    routes a route server learned through the association the route server holds, so what
//!    is missing is the route server's own, not a pairing of a route table with a VPC, and
//!    neither the route table the request named nor a VPC it is not associated with is part of
//!    the sentence;
//! 2. `DryRun` is answered once the request's ids have been resolved and BEFORE the pair's own
//!    state is asked about: the flag "checks whether you have the required permissions for the
//!    action without actually making the request", a question about the CALLER, so an
//!    authorized probe of a write whose state precondition does not hold is answered 412
//!    `DryRunOperation` with EC2's fixed sentence rather than with that state conflict — while
//!    a request whose ids or association cannot be resolved is still refused on those terms,
//!    there being nothing to probe the permissions on. No probe writes.
//!
//! Each row is a distinct input along what those rules generalise over — why the association
//! is missing (never made, made with another VPC, taken apart), which route table and VPC the
//! request pairs the route server with, which of the two writes is asked, and the state the
//! pair is in when the probe is made (propagating and not yet in force, propagating and in
//! force, never enabled, disabled and carried out) — so no one row is the specification.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message};
use super::rbin_retention_rules::poll_until;
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

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
async fn create_route_server(case: &str, router: &MultiServiceRouter, amazon_side_asn: &str) -> String {
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

/// The form fields a (route server, VPC) pairing carries.
fn pairing(route_server_id: &str, vpc_id: &str) -> Vec<Param> {
    vec![
        param("RouteServerId", route_server_id),
        param("VpcId", vpc_id),
    ]
}

/// `AssociateRouteServer` for one pair, which must succeed: the association is what the
/// propagation rules are defined against.
async fn associate(case: &str, router: &MultiServiceRouter, route_server_id: &str, vpc_id: &str) {
    ok(
        case,
        router,
        "AssociateRouteServer",
        &pairing(route_server_id, vpc_id),
    )
    .await;
}

/// The form fields the two propagation writes share, plus whatever the case adds.
fn pair_params(route_server_id: &str, route_table_id: &str, extra: &[Param]) -> Vec<Param> {
    let mut params = vec![
        param("RouteServerId", route_server_id),
        param("RouteTableId", route_table_id),
    ];
    params.extend_from_slice(extra);
    params
}

/// The `(routeTableId, state)` pairs `GetRouteServerPropagations` reports for
/// `route_server_id`, in the order the report carries them.
async fn reported_pairs(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
) -> Vec<(String, String)> {
    let body = ok(
        case,
        router,
        "GetRouteServerPropagations",
        &[param("RouteServerId", route_server_id)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| {
            node.is_element()
                && child(*node, "routeServerId").is_some()
                && child(*node, "routeTableId").is_some()
        })
        .map(|node| {
            (
                child_text(node, "routeTableId").unwrap_or_default(),
                child_text(node, "state").unwrap_or_default(),
            )
        })
        .collect()
}

/// The state `GetRouteServerPropagations` reports for one pair, or `None` when the pair does
/// not propagate at all.
async fn reported_state(
    case: &str,
    router: &MultiServiceRouter,
    route_server_id: &str,
    route_table_id: &str,
) -> Option<String> {
    reported_pairs(case, router, route_server_id)
        .await
        .into_iter()
        .find(|(table, _)| table == route_table_id)
        .map(|(_, state)| state)
}

// ---------------------------------------------------------------------------
// Rule 1: the refusal names the route server and the association it lacks
// ---------------------------------------------------------------------------

/// RULE 1: a propagation write whose route server holds no association with the VPC the named
/// route table routes for is refused `InvalidRouteServerId.NotAssociated` with the sentence
/// about THAT ROUTE SERVER, naming neither the route table nor any VPC.
///
/// The rows differ in why the association is missing (never made, made with another VPC,
/// taken apart by a disassociation), in the route table and VPC the request pairs the route
/// server with, and in which of the two writes is asked.
#[tokio::test]
async fn a_propagation_refusal_names_the_route_server_and_the_association_it_lacks() {
    let router = emulator();

    let never_associated = create_route_server("a route server never associated", &router, "64512").await;
    let associated_elsewhere = create_route_server("a route server associated elsewhere", &router, "64513").await;
    let taken_apart = create_route_server("a route server whose association was taken apart", &router, "64514").await;

    let first_vpc = create_vpc("the first VPC", &router, "10.61.0.0/16").await;
    let second_vpc = create_vpc("the second VPC", &router, "10.62.0.0/16").await;
    let third_vpc = create_vpc("the third VPC", &router, "10.63.0.0/16").await;

    let first_table = create_route_table("a table of the first VPC", &router, &first_vpc).await;
    let second_table = create_route_table("a second table of the first VPC", &router, &first_vpc).await;
    let third_table = create_route_table("a table of the second VPC", &router, &second_vpc).await;

    associate(
        "the route server associated elsewhere",
        &router,
        &associated_elsewhere,
        &third_vpc,
    )
    .await;
    associate(
        "the route server whose association is taken apart",
        &router,
        &taken_apart,
        &first_vpc,
    )
    .await;
    // A disassociation holds the pair apart from the call on, so this route server serves the
    // first VPC's tables no longer even before the clock has carried the teardown out.
    ok(
        "the association taken apart",
        &router,
        "DisassociateRouteServer",
        &pairing(&taken_apart, &first_vpc),
    )
    .await;

    // (case, the route server, the route table, the VPC that table routes for)
    let rows: [(&str, &str, &str, &str); 4] = [
        (
            "a route server with no association at all",
            &never_associated,
            &first_table,
            &first_vpc,
        ),
        (
            "a route server with no association, named with another table",
            &never_associated,
            &third_table,
            &second_vpc,
        ),
        (
            "a route server associated with another VPC",
            &associated_elsewhere,
            &second_table,
            &first_vpc,
        ),
        (
            "a route server whose association was taken apart",
            &taken_apart,
            &first_table,
            &first_vpc,
        ),
    ];

    for (case, route_server_id, route_table_id, vpc_id) in rows {
        for name in ["EnableRouteServerPropagation", "DisableRouteServerPropagation"] {
            // The flag changes nothing about THIS refusal: an association that is not there
            // is not a permission to probe.
            for flag in [Vec::new(), vec![param("DryRun", "true")]] {
                let probed = if flag.is_empty() { "the request" } else { "a dry run of it" };
                let (status, body) =
                    action(&router, name, &pair_params(route_server_id, route_table_id, &flag)).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400 from {name} ({probed}), got {status} \
                     with body {body}"
                );
                assert_eq!(
                    error_code(&body),
                    "InvalidRouteServerId.NotAssociated",
                    "case {case:?}: expected InvalidRouteServerId.NotAssociated from {name} \
                     ({probed}), got body {body}"
                );
                let message = error_message(&body);
                assert_eq!(
                    message,
                    format!("Route Server {route_server_id} is not associated with a VPC."),
                    "case {case:?}: expected {name} ({probed}) to name only the route \
                     server, got body {body}"
                );
                assert!(
                    !message.contains(route_table_id) && !message.contains(vpc_id),
                    "case {case:?}: the refusal from {name} ({probed}) must name neither \
                     {route_table_id} nor {vpc_id}, got {message:?}"
                );
            }
        }

        // Nothing was written for a pair the service refused.
        let reported = reported_pairs(case, &router, route_server_id).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: a refused request must write no propagation, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the probe is answered before the pair's own state
// ---------------------------------------------------------------------------

/// RULE 2: a `DryRun` of either propagation write is answered `DryRunOperation` even where the
/// pair's own state would have refused the request, and it writes nothing — while the same
/// request without the flag is still refused `IncorrectState`.
///
/// The rows differ in which write is probed and in the state the pair is in when the probe is
/// made: propagating but not yet in force, propagating and in force, never enabled, and
/// disabled with the disable carried out.
#[tokio::test]
async fn a_dry_run_of_a_propagation_write_is_answered_before_the_pairs_state() {
    let router = emulator();
    let route_server_id = create_route_server("the route server", &router, "64520").await;
    let vpc_id = create_vpc("the VPC", &router, "10.64.0.0/16").await;
    associate("the route server with the VPC", &router, &route_server_id, &vpc_id).await;

    let not_yet_in_force = create_route_table("the table enabled just now", &router, &vpc_id).await;
    let in_force = create_route_table("the table whose enable settled", &router, &vpc_id).await;
    let never_enabled = create_route_table("a table never enabled", &router, &vpc_id).await;
    let disabled = create_route_table("the table whose disable was carried out", &router, &vpc_id).await;

    // One probe: the case, the operation, and the route table the pair names.
    type ProbeRow<'a> = (&'a str, &'a str, &'a str);
    let mut rows: Vec<ProbeRow<'_>> = Vec::new();

    // A pair enabled a moment ago is propagating and NOT yet in force, which is the state a
    // caller reading straight after the enable sees.
    ok(
        "the enable of the table probed while pending",
        &router,
        "EnableRouteServerPropagation",
        &pair_params(&route_server_id, &not_yet_in_force, &[]),
    )
    .await;
    assert_eq!(
        reported_state("the pair enabled just now", &router, &route_server_id, &not_yet_in_force).await,
        Some(ec2::state::ROUTE_SERVER_PROPAGATION_STATE_PENDING.to_string()),
        "the pair enabled a moment ago must still be reported as not yet in force"
    );
    rows.push((
        "an enable of a pair propagating but not yet in force",
        "EnableRouteServerPropagation",
        &not_yet_in_force,
    ));

    // ...and one whose enable the CLOCK has settled is in force. Waited for by looking at
    // the pair, never by a span.
    ok(
        "the enable of the table probed once in force",
        &router,
        "EnableRouteServerPropagation",
        &pair_params(&route_server_id, &in_force, &[]),
    )
    .await;
    {
        let probe_router = &router;
        let probe_server = route_server_id.as_str();
        let probe_table = in_force.as_str();
        poll_until("the enabled pair", "the propagation to come into force", || async move {
            reported_state("the enabled pair", probe_router, probe_server, probe_table).await
                == Some(ec2::state::ROUTE_SERVER_PROPAGATION_STATE_AVAILABLE.to_string())
        })
        .await;
    }
    rows.push((
        "an enable of a pair already in force",
        "EnableRouteServerPropagation",
        &in_force,
    ));

    // A pair nothing ever enabled has no disable to perform.
    rows.push((
        "a disable of a pair that never propagated",
        "DisableRouteServerPropagation",
        &never_enabled,
    ));

    // ...and neither has one whose disable the clock has already carried out, after which the
    // pair propagates no longer and is reported no more.
    ok(
        "the enable of the table later disabled",
        &router,
        "EnableRouteServerPropagation",
        &pair_params(&route_server_id, &disabled, &[]),
    )
    .await;
    ok(
        "the disable of that table",
        &router,
        "DisableRouteServerPropagation",
        &pair_params(&route_server_id, &disabled, &[]),
    )
    .await;
    {
        let probe_router = &router;
        let probe_server = route_server_id.as_str();
        let probe_table = disabled.as_str();
        poll_until("the disabled pair", "the disable to be carried out", || async move {
            reported_state("the disabled pair", probe_router, probe_server, probe_table)
                .await
                .is_none()
        })
        .await;
    }
    rows.push((
        "a disable of a pair whose disable was carried out",
        "DisableRouteServerPropagation",
        &disabled,
    ));

    let before = reported_pairs("before the probes", &router, &route_server_id).await;

    for (case, name, route_table_id) in rows {
        let (status, body) = action(
            &router,
            name,
            &pair_params(&route_server_id, route_table_id, &[param("DryRun", "true")]),
        )
        .await;
        assert_eq!(
            status, ec2::state::DRY_RUN_OPERATION_STATUS,
            "case {case:?}: expected HTTP {expected} from {name}, got {status} with body \
             {body}",
            expected = ec2::state::DRY_RUN_OPERATION_STATUS
        );
        assert_eq!(
            error_code(&body),
            ec2::state::DRY_RUN_OPERATION,
            "case {case:?}: expected {expected} from {name}, got body {body}",
            expected = ec2::state::DRY_RUN_OPERATION
        );
        assert_eq!(
            error_message(&body),
            ec2::state::DRY_RUN_OPERATION_MESSAGE,
            "case {case:?}: expected EC2's dry run sentence from {name}, got body {body}"
        );

        // The state the flag was answered ahead of is still the answer without it.
        let (plain_status, plain_body) = action(
            &router,
            name,
            &pair_params(&route_server_id, route_table_id, &[]),
        )
        .await;
        assert_eq!(
            plain_status, 400,
            "case {case:?}: expected HTTP 400 from {name} without the flag, got \
             {plain_status} with body {plain_body}"
        );
        assert_eq!(
            error_code(&plain_body),
            "IncorrectState",
            "case {case:?}: expected IncorrectState from {name} without the flag, got body \
             {plain_body}"
        );
    }

    // No probe, and no refusal, wrote anything: the propagations are the ones the enables
    // recorded and the clock settled.
    let after = reported_pairs("after the probes", &router, &route_server_id).await;
    assert_eq!(
        after, before,
        "a probe must write nothing, got {after:?} where {before:?} was reported before them"
    );
    let mut tables: Vec<String> = after.into_iter().map(|(table, _)| table).collect();
    tables.sort();
    let mut expected = vec![not_yet_in_force.clone(), in_force.clone()];
    expected.sort();
    assert_eq!(
        tables, expected,
        "only the two enabled pairs may propagate, got {tables:?}"
    );
}
