// SPDX-License-Identifier: CC0-1.0

//! What a transit gateway a delete has taken away still answers, driven over the wire the
//! service speaks (`ec2Query`, an `application/x-www-form-urlencoded` POST answered with XML;
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a resource whose lifecycle ends in a state the service REPORTS it in
//! stays readable in that state instead of being dropped, so a read still answers with it and
//! a repeat of the call that ended it is accepted and answers it in that state; only an id the
//! account has never held one for is refused not-found. For a transit gateway that terminal
//! state is the `TransitGatewayState` catalog's `deleted`: `DescribeTransitGateways` reports
//! the gateway in it and a repeat `DeleteTransitGateway` answers it, while the gateway takes no
//! further work — nothing can be created on it any more — and the route tables that came up
//! with it are gone.
//!
//! The rows span what that generalises over: how the gateway was created (bare, described and
//! tagged, carrying an additional route table), WHEN the repeat delete arrives (while the move
//! is still owed and once it has been made) and how often it is repeated.
//!
//! `DryRun` is on the same rule from the other side: the flag is answered before the gateway is
//! touched, so a dry run of the delete answers the verdict and leaves the gateway exactly as it
//! was — whether it is still in service or already deleted.

use std::time::Duration;

use ec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS,
    INVALID_TRANSIT_GATEWAY_ID_MALFORMED, INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
    TRANSIT_GATEWAY_STATE_AVAILABLE, TRANSIT_GATEWAY_STATE_DELETED, TRANSIT_GATEWAY_STATE_DELETING,
    TRANSIT_GATEWAY_STATE_PENDING,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the deadline
/// the polls below give up at. Generous, because what is under test is the state a resource
/// SETTLES in rather than how fast it gets there.
const MOVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll below waits between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A well-formed transit gateway id no create ever minted: the mint counts from one, so a high
/// identifier names nothing this account has ever held.
const NEVER_MINTED: &str = "tgw-0fedcba9876543210";

/// A value that is no transit gateway id at all.
const NOT_AN_ID: &str = "tgw-not-an-id";

/// One `action` call signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// The same call, requiring HTTP 200, and the body it answered.
async fn invoke_ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One transit gateway created with `params`, as its id.
async fn transit_gateway(case: &str, router: &MultiServiceRouter, params: &[Param]) -> String {
    let body = invoke_ok(case, router, "CreateTransitGateway", params).await;
    element(&body, "transitGatewayId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must report the id it minted: {body}"))
}

/// One `DeleteTransitGateway` of `transit_gateway_id`, with `extra` form fields beside the id.
async fn delete(
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
    extra: &[Param],
) -> (u16, String) {
    let mut params = vec![param("TransitGatewayId", transit_gateway_id)];
    params.extend_from_slice(extra);
    invoke(router, "DeleteTransitGateway", &params).await
}

/// The state `DescribeTransitGateways` reports for `transit_gateway_id`, requiring the report to
/// carry the gateway at all.
async fn reported_state(case: &str, router: &MultiServiceRouter, transit_gateway_id: &str) -> String {
    let body = invoke_ok(
        case,
        router,
        "DescribeTransitGateways",
        &[param("TransitGatewayIds.1", transit_gateway_id)],
    )
    .await;
    assert_eq!(
        element(&body, "transitGatewayId").as_deref(),
        Some(transit_gateway_id),
        "case {case:?}: the report must carry {transit_gateway_id}, got {body}"
    );
    element(&body, "state")
        .unwrap_or_else(|| panic!("case {case:?}: the reported gateway must carry a state: {body}"))
}

/// Look again until `transit_gateway_id` is reported in `wanted`, or a generous deadline passes.
///
/// The CONDITION is what is waited for, never a span of the clock, so nothing here depends on
/// how fast the host applies a move the clock owes.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
    wanted: &str,
) {
    let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
    loop {
        let state = reported_state(case, router, transit_gateway_id).await;
        if state == wanted {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {transit_gateway_id} must reach {wanted:?}, and it reported {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the retention table: how the gateway was created and how often its delete is
/// repeated.
struct Row {
    case: &'static str,
    /// The create's own form fields, so no two rows store the same gateway.
    created_with: &'static [(&'static str, &'static str)],
    /// Whether the row gives the gateway an additional route table of its own, which the
    /// deletion takes with it.
    extra_route_table: bool,
    /// How many times the delete is repeated once the gateway has reached its terminal state.
    repeats: usize,
}

/// A deleted transit gateway stays readable in `deleted`, and every repeat of its delete is
/// accepted and answers it in the state it is in.
#[tokio::test]
async fn a_deleted_transit_gateway_stays_readable_and_its_delete_repeats() {
    let rows = [
        Row {
            case: "a bare gateway, deleted once more as soon as it is gone",
            created_with: &[],
            extra_route_table: false,
            repeats: 1,
        },
        Row {
            case: "a described and tagged gateway, deleted three times more",
            created_with: &[
                ("Description", "the gateway a teardown deletes twice"),
                ("TagSpecification.1.ResourceType", "transit-gateway"),
                ("TagSpecification.1.Tag.1.Key", "Tier"),
                ("TagSpecification.1.Tag.1.Value", "retention"),
            ],
            extra_route_table: false,
            repeats: 3,
        },
        Row {
            case: "a gateway carrying a route table of its own",
            created_with: &[("Options.VpnEcmpSupport", "disable")],
            extra_route_table: true,
            repeats: 2,
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    for row in &rows {
        let Row {
            case,
            created_with,
            extra_route_table,
            repeats,
        } = row;
        let created: Vec<Param> = created_with
            .iter()
            .map(|(name, value)| param(name, value))
            .collect();
        let gateway = transit_gateway(case, &router, &created).await;
        let mut tables: Vec<String> = Vec::new();
        if *extra_route_table {
            let body = invoke_ok(
                case,
                &router,
                "CreateTransitGatewayRouteTable",
                &[param("TransitGatewayId", &gateway)],
            )
            .await;
            tables.push(element(&body, "transitGatewayRouteTableId").unwrap_or_else(|| {
                panic!("case {case:?}: the create must report the table it minted: {body}")
            }));
        }

        // The delete itself answers the TRANSIENT state, as it did before anything was retained.
        let body = invoke_ok(case, &router, "DeleteTransitGateway", &[param("TransitGatewayId", &gateway)]).await;
        assert_eq!(
            element(&body, "state").as_deref(),
            Some(TRANSIT_GATEWAY_STATE_DELETING),
            "case {case:?}: the delete must answer the transient state it put the gateway in, \
             got {body}"
        );

        // A repeat that arrives before the move has been made is accepted too: the state it
        // answers is whichever of the two the gateway is in by then, never a refusal.
        let (status, body) = delete(&router, &gateway, &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a repeat delete must be accepted, got {status} with body {body}"
        );
        let repeated = element(&body, "state").unwrap_or_default();
        assert!(
            repeated == TRANSIT_GATEWAY_STATE_DELETING || repeated == TRANSIT_GATEWAY_STATE_DELETED,
            "case {case:?}: a repeat delete must answer the gateway's own state, got \
             {repeated:?} in {body}"
        );
        assert_eq!(
            element(&body, "transitGatewayId").as_deref(),
            Some(gateway.as_str()),
            "case {case:?}: the repeat must answer the gateway the request named, got {body}"
        );

        // Once the clock has finished the move the gateway is in its TERMINAL state — and still
        // reported, which is the whole of what that state means.
        poll_until_state(case, &router, &gateway, TRANSIT_GATEWAY_STATE_DELETED).await;
        for repeat in 0..*repeats {
            let (status, body) = delete(&router, &gateway, &[]).await;
            assert_eq!(
                status, 200,
                "case {case:?}: repeat {repeat} must be accepted, got {status} with body {body}"
            );
            assert_eq!(
                element(&body, "state").as_deref(),
                Some(TRANSIT_GATEWAY_STATE_DELETED),
                "case {case:?}: repeat {repeat} must answer the terminal state, got {body}"
            );
            assert_eq!(
                element(&body, "transitGatewayId").as_deref(),
                Some(gateway.as_str()),
                "case {case:?}: repeat {repeat} must answer the gateway named, got {body}"
            );
        }
        assert_eq!(
            reported_state(case, &router, &gateway).await,
            TRANSIT_GATEWAY_STATE_DELETED,
            "case {case:?}: no repeat may move the gateway out of its terminal state"
        );

        // The route tables that came up with the gateway are not retained with it: a table is a
        // resource OF the gateway, so the deletion takes it.
        let body = invoke_ok(case, &router, "DescribeTransitGatewayRouteTables", &[]).await;
        let reported = elements(&body, "transitGatewayRouteTableId");
        for table in &tables {
            assert!(
                !reported.contains(table),
                "case {case:?}: the table {table} of a deleted gateway must stop being reported, \
                 got {reported:?}"
            );
        }
    }
}

/// A gateway in its terminal state takes NO further work: every create that would make a
/// resource of it, and the modify of it, are refused for an id the account no longer holds.
#[tokio::test]
async fn a_deleted_transit_gateway_takes_no_further_work() {
    let case = "a gateway whose delete has been made";
    let (router, _engine) = super::ec2_query::emulator();
    let gateway = transit_gateway(case, &router, &[]).await;
    let body = invoke_ok(
        case,
        &router,
        "CreateVpc",
        &[param("CidrBlock", "10.9.0.0/16")],
    )
    .await;
    let vpc_id = element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId"));
    let body = invoke_ok(
        case,
        &router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.9.1.0/24"),
            param("AvailabilityZone", &format!("{REGION}a")),
        ],
    )
    .await;
    let subnet_id = element(&body, "subnetId").unwrap_or_else(|| panic!("case {case:?}: no subnetId"));

    delete(&router, &gateway, &[]).await;
    poll_until_state(case, &router, &gateway, TRANSIT_GATEWAY_STATE_DELETED).await;

    // (the call, the fields it names beside the gateway)
    let refused: Vec<(&str, Vec<Param>)> = vec![
        ("CreateTransitGatewayRouteTable", vec![]),
        ("CreateTransitGatewayPolicyTable", vec![]),
        (
            "CreateTransitGatewayVpcAttachment",
            vec![param("VpcId", &vpc_id), param("SubnetIds.1", &subnet_id)],
        ),
        (
            "ModifyTransitGateway",
            vec![param("Description", "a gateway that is gone")],
        ),
    ];
    for (action, extra) in &refused {
        let mut params = vec![param("TransitGatewayId", &gateway)];
        params.extend_from_slice(extra);
        let (status, body) = invoke(&router, action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {action} on a deleted gateway must be refused, got {status} with \
             body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            "case {case:?}: {action} must answer the gateway's own not-found code, got {code:?} \
             in {body}"
        );
    }
}

/// Only an id the account has NEVER held a gateway for is refused: the not-found half of the
/// rule, beside the form half that needs no lookup at all.
#[tokio::test]
async fn an_id_the_account_never_held_is_still_refused() {
    let case = "an id no create ever minted";
    let (router, _engine) = super::ec2_query::emulator();

    // (the call, the id, the expected code)
    let rows: &[(&str, &str, &str)] = &[
        (
            "DeleteTransitGateway",
            NEVER_MINTED,
            INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
        ),
        (
            "DescribeTransitGateways",
            NEVER_MINTED,
            INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
        ),
        (
            "DeleteTransitGateway",
            NOT_AN_ID,
            INVALID_TRANSIT_GATEWAY_ID_MALFORMED,
        ),
        (
            "DescribeTransitGateways",
            NOT_AN_ID,
            INVALID_TRANSIT_GATEWAY_ID_MALFORMED,
        ),
    ];
    for (action, id, expected) in rows {
        let field = if *action == "DeleteTransitGateway" {
            "TransitGatewayId"
        } else {
            "TransitGatewayIds.1"
        };
        let (status, body) = invoke(&router, action, &[param(field, id)]).await;
        assert_eq!(
            status, 400,
            "case {case:?}: {action} naming {id} must be refused, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, *expected,
            "case {case:?}: {action} naming {id} must answer {expected:?}, got {code:?} in {body}"
        );
        assert!(
            error_message(&body).contains(id),
            "case {case:?}: the refusal must name the value the request sent, got {:?}",
            error_message(&body)
        );
    }
}

/// A terminal record is kept so a read and a repeat delete can still answer with it, not for
/// ever: the store holds at most [`ec2::state::MAX_TRANSIT_GATEWAYS`] gateways in all, and the
/// quota a create is held to counts only the gateways the account HOLDS — so a teardown
/// followed by a create succeeds at the ceiling, and the record of the gateway that is gone is
/// what the create takes its room from.
#[tokio::test]
async fn a_create_at_the_ceiling_takes_the_room_of_a_deleted_gateway() {
    let case = "a create at the ceiling";
    let (router, engine) = super::ec2_query::emulator();
    let store = engine.state::<ec2::state::ComAmazonawsEc2Amazonec2State>();

    // One short of the ceiling, seeded through the one writer every create goes through.
    {
        let mut guard = store.lock();
        for index in 0..(ec2::state::MAX_TRANSIT_GATEWAYS - 1) {
            guard.insert_transit_gateway(ec2::state::TransitGatewaySpec {
                description: Some(format!("seeded {index}")),
                region: REGION.to_string(),
                created_at: std::time::SystemTime::now(),
                options: ec2::state::transit_gateway_options::<()>(None)
                    .expect("the documented defaults resolve"),
                tags: Vec::new(),
            });
        }
    }
    // The create that fills the ceiling, then its own delete, carried through to the terminal
    // state: the store is then AT the ceiling with one record of a gateway that is gone.
    let filling = transit_gateway(case, &router, &[]).await;
    delete(&router, &filling, &[]).await;
    poll_until_state(case, &router, &filling, TRANSIT_GATEWAY_STATE_DELETED).await;

    // So the create is served: the quota counts what the account holds, and the room comes from
    // the record of the deleted gateway rather than from growing the store.
    let replacement = transit_gateway(case, &router, &[]).await;
    assert_ne!(
        replacement, filling,
        "case {case:?}: the create must mint a fresh gateway, got the deleted one"
    );
    let held = store.lock().transit_gateways.len();
    assert!(
        held <= ec2::state::MAX_TRANSIT_GATEWAYS,
        "case {case:?}: the store must hold at most {} gateways, got {held}",
        ec2::state::MAX_TRANSIT_GATEWAYS
    );
    assert_eq!(
        reported_state(case, &router, &replacement).await,
        TRANSIT_GATEWAY_STATE_PENDING,
        "case {case:?}: the gateway the create minted must be readable"
    );
}

/// `DryRun` is answered before the gateway is touched: the verdict is the same for a gateway in
/// service and for one already deleted, and neither is moved by the probe.
#[tokio::test]
async fn a_dry_run_delete_moves_no_gateway() {
    let case = "a dry run of a delete";
    let (router, _engine) = super::ec2_query::emulator();
    let live = transit_gateway(case, &router, &[]).await;
    poll_until_state(case, &router, &live, TRANSIT_GATEWAY_STATE_AVAILABLE).await;
    let gone = transit_gateway(case, &router, &[]).await;
    delete(&router, &gone, &[]).await;
    poll_until_state(case, &router, &gone, TRANSIT_GATEWAY_STATE_DELETED).await;

    for (named, held) in [
        (&live, TRANSIT_GATEWAY_STATE_AVAILABLE),
        (&gone, TRANSIT_GATEWAY_STATE_DELETED),
    ] {
        let (status, body) = delete(&router, named, &[param("DryRun", "true")]).await;
        assert_eq!(
            status, DRY_RUN_OPERATION_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS} for a dry run of {named}, \
             got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, DRY_RUN_OPERATION,
            "case {case:?}: expected the code {DRY_RUN_OPERATION:?}, got {code:?} in {body}"
        );
        assert_eq!(
            error_message(&body),
            DRY_RUN_OPERATION_MESSAGE,
            "case {case:?}: the verdict carries its own fixed sentence, got {body}"
        );
        assert_eq!(
            reported_state(case, &router, named).await,
            held,
            "case {case:?}: a dry run must leave {named} in the state it was in"
        );
    }

    // The control: the very same delete without the flag does move the gateway, so the
    // assertions above are about the flag rather than about a delete that never works.
    let body = invoke_ok(
        case,
        &router,
        "DeleteTransitGateway",
        &[param("TransitGatewayId", &live)],
    )
    .await;
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(TRANSIT_GATEWAY_STATE_DELETING),
        "case {case:?}: the delete the dry run stood for must work, got {body}"
    );
}
