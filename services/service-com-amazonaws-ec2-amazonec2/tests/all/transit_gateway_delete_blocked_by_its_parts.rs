// SPDX-License-Identifier: CC0-1.0

//! The refusal a transit gateway delete answers while the resource still carries the parts
//! the CALLER takes away first, driven over `ec2Query` (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test: a transit gateway resource whose own state forbids the delete — a
//! gateway that still carries attachments, a route table attachments are still associated
//! with — is refused HTTP 400 `IncorrectState`, the code a client reads as "clear what holds
//! it and ask again", with a message that OPENS with the resource id the request named,
//! carries that blocker's fixed text and names no dependent id; and the resource is left
//! exactly as it was, still readable and still in the state it held, with its parts intact.
//!
//! The rows span what that generalises over: HOW MANY parts hold the resource (one, two),
//! WHEN the delete arrives (as soon as the part was added, and once the clock has settled
//! both the resource and the part into their steady states) and WHICH member of the family
//! is deleted (the route table its associations hold, the gateway its attachments hold) —
//! and each test ends on the control the rule needs to be about the parts at all: the same
//! delete of the same kind of resource that nothing holds succeeds.
//!
//! The last test closes the loop the refusal points a client at:
//! `DisassociateTransitGatewayRouteTable` takes an association down — transiently, with the
//! clock owing the removal — and the delete that was refused while the association was there
//! succeeds once it is gone, which is read back through
//! `GetTransitGatewayRouteTableAssociations` rather than assumed. Its refusal rows are one
//! per way that call can fail to resolve what it was given.

use std::time::Duration;

use ec2::state::{
    TRANSIT_GATEWAY_ASSOCIATION_STATE_ASSOCIATED, TRANSIT_GATEWAY_ASSOCIATION_STATE_DISASSOCIATING,
    TRANSIT_GATEWAY_ROUTE_TABLE_STATE_AVAILABLE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The state a delete that WAS admissible answers the resource it removed with.
const DELETING: &str = "deleting";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the
/// deadline the polls below give up at. Generous, because what is under test is the state a
/// resource SETTLES in rather than how fast it gets there — a tight budget here would make
/// the suite depend on how fast the host runs.
const MOVE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll below waits between two looks at a resource.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

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

/// The text of `name` in `body`, or a panic naming the case when the answer carries none.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: the answer must carry {name}, got {body}"))
}

/// A fresh transit gateway of the account, as its id.
async fn transit_gateway(case: &str, router: &MultiServiceRouter) -> String {
    let body = invoke_ok(case, router, "CreateTransitGateway", &[]).await;
    member(case, &body, "transitGatewayId")
}

/// A VPC with one subnet in it, as `(vpc id, subnet id)`.
///
/// `nth` picks the CIDR, so every network of a run is distinct: one VPC attaches to one
/// transit gateway once (`CreateTransitGatewayVpcAttachment`), so a row that wants two
/// attachments needs two networks.
async fn network(case: &str, router: &MultiServiceRouter, nth: u8) -> (String, String) {
    let body = invoke_ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", &format!("10.{nth}.0.0/16"))],
    )
    .await;
    let vpc_id = member(case, &body, "vpcId");
    let body = invoke_ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", &format!("10.{nth}.1.0/24")),
            param("AvailabilityZone", &format!("{REGION}a")),
        ],
    )
    .await;
    let subnet_id = member(case, &body, "subnetId");
    (vpc_id, subnet_id)
}

/// One VPC attachment of `transit_gateway_id`, made through the wire on a network of its
/// own, as its id.
async fn attachment(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
    nth: u8,
) -> String {
    let (vpc_id, subnet_id) = network(case, router, nth).await;
    let body = invoke_ok(
        case,
        router,
        "CreateTransitGatewayVpcAttachment",
        &[
            param("TransitGatewayId", transit_gateway_id),
            param("VpcId", &vpc_id),
            param("SubnetIds.1", &subnet_id),
        ],
    )
    .await;
    member(case, &body, "transitGatewayAttachmentId")
}

/// An additional route table of `transit_gateway_id`, as its id.
async fn route_table(case: &str, router: &MultiServiceRouter, transit_gateway_id: &str) -> String {
    let body = invoke_ok(
        case,
        router,
        "CreateTransitGatewayRouteTable",
        &[param("TransitGatewayId", transit_gateway_id)],
    )
    .await;
    member(case, &body, "transitGatewayRouteTableId")
}

/// The state `DescribeTransitGatewayRouteTables` reports for one route table, or `None` when
/// it reports no such table at all.
async fn reported_table_state(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> Option<String> {
    let body = invoke_ok(
        case,
        router,
        "DescribeTransitGatewayRouteTables",
        &[param("TransitGatewayRouteTableIds.1", route_table_id)],
    )
    .await;
    element(&body, "state")
}

/// Look again until `route_table_id` is reported in `wanted`, or a generous deadline passes.
///
/// The CONDITION is what is waited for, never a span of the clock, so the suite cannot
/// depend on how fast the host applies a move the clock owes.
async fn poll_until_table_state(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    wanted: &str,
) {
    let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
    loop {
        let state = reported_table_state(case, router, route_table_id).await;
        if state.as_deref() == Some(wanted) {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {route_table_id} must reach {wanted:?}, and it reported {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The `(attachment id, state)` pairs `GetTransitGatewayRouteTableAssociations` reports for
/// one route table, in the order it reports them.
async fn reported_associations(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
) -> Vec<(String, String)> {
    let body = invoke_ok(
        case,
        router,
        "GetTransitGatewayRouteTableAssociations",
        &[param("TransitGatewayRouteTableId", route_table_id)],
    )
    .await;
    elements(&body, "transitGatewayAttachmentId")
        .into_iter()
        .zip(elements(&body, "state"))
        .collect()
}

/// Look again until every association of `route_table_id` reports `associated`, or a generous
/// deadline passes.
async fn poll_until_associations_settled(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    expected: usize,
) {
    let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
    loop {
        let reported = reported_associations(case, router, route_table_id).await;
        if reported.len() == expected
            && reported
                .iter()
                .all(|(_, state)| state == TRANSIT_GATEWAY_ASSOCIATION_STATE_ASSOCIATED)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {route_table_id} must settle {expected} associations, and it \
             reported {reported:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The attachment ids `DescribeTransitGatewayAttachments` reports for the ids `wanted` names,
/// and the states it reports them in.
async fn reported_attachments(
    case: &str,
    router: &MultiServiceRouter,
    wanted: &[String],
) -> (Vec<String>, Vec<String>) {
    let params: Vec<Param> = wanted
        .iter()
        .enumerate()
        .map(|(index, attachment_id)| {
            param(
                &format!("TransitGatewayAttachmentIds.{}", index + 1),
                attachment_id,
            )
        })
        .collect();
    let body = invoke_ok(
        case,
        router,
        "DescribeTransitGatewayAttachments",
        &params,
    )
    .await;
    (
        elements(&body, "transitGatewayAttachmentId"),
        elements(&body, "state"),
    )
}

/// The state `DescribeTransitGateways` reports for one transit gateway, or `None` when it
/// reports no such gateway at all.
async fn reported_gateway_state(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
) -> Option<String> {
    let body = invoke_ok(
        case,
        router,
        "DescribeTransitGateways",
        &[param("TransitGatewayIds.1", transit_gateway_id)],
    )
    .await;
    element(&body, "state")
}

/// One row of the route-table table: how many attachments hold the table, and whether the
/// clock is let settle the table and its associations before the delete is asked.
struct TableCase {
    name: &'static str,
    associations: u8,
    settled: bool,
}

/// A route table attachments are still associated with is refused `IncorrectState` naming the
/// table the request named, nothing of it is removed, and a table nothing holds is deletable.
#[tokio::test]
async fn a_route_table_its_associations_hold_is_refused_for_its_state() {
    let cases = [
        TableCase {
            name: "one association, asked as soon as it was made",
            associations: 1,
            settled: false,
        },
        TableCase {
            name: "one association, asked once the table and the association have settled",
            associations: 1,
            settled: true,
        },
        TableCase {
            name: "two associations, asked once the table and both have settled",
            associations: 2,
            settled: true,
        },
    ];

    let (router, _engine) = super::ec2_query::emulator();
    let gateway = transit_gateway("the gateway every row's table belongs to", &router).await;
    // One attachment per association any row needs: an attachment holds at most one route
    // table, so no two rows can share one.
    let mut minted: u8 = 0;

    for case in &cases {
        let TableCase {
            name,
            associations,
            settled,
        } = case;
        let table = route_table(name, &router, &gateway).await;
        let mut holders: Vec<String> = Vec::new();
        for _ in 0..*associations {
            minted += 1;
            let holder = attachment(name, &router, &gateway, minted).await;
            invoke_ok(
                name,
                &router,
                "AssociateTransitGatewayRouteTable",
                &[
                    param("TransitGatewayRouteTableId", &table),
                    param("TransitGatewayAttachmentId", &holder),
                ],
            )
            .await;
            holders.push(holder);
        }
        if *settled {
            poll_until_table_state(
                name,
                &router,
                &table,
                TRANSIT_GATEWAY_ROUTE_TABLE_STATE_AVAILABLE,
            )
            .await;
            poll_until_associations_settled(name, &router, &table, holders.len()).await;
        }

        let (status, body) = invoke(
            &router,
            "DeleteTransitGatewayRouteTable",
            &[param("TransitGatewayRouteTableId", &table)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: a table its associations hold must be refused, got {status} with \
             body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code,
            ec2::state::INCORRECT_STATE,
            "case {name:?}: expected the code a state that forbids the delete carries, got \
             {code:?} in {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            format!("{table} has associated attachments"),
            "case {name:?}: the refusal must name the table the request named, got {message:?}"
        );
        for holder in &holders {
            assert!(
                !message.contains(holder.as_str()),
                "case {name:?}: the refusal must name no attachment id, and it names \
                 {holder:?}: {message:?}"
            );
        }

        // Nothing was removed: the table reads back in the state it held, and every
        // association is still there to be cleared.
        let state = reported_table_state(name, &router, &table).await;
        assert_ne!(
            state.as_deref(),
            Some(DELETING),
            "case {name:?}: a refused delete must not move the table towards removal, got \
             {state:?}"
        );
        if *settled {
            assert_eq!(
                state.as_deref(),
                Some(TRANSIT_GATEWAY_ROUTE_TABLE_STATE_AVAILABLE),
                "case {name:?}: a refused delete must leave the table in service, got {state:?}"
            );
        }
        let mut held: Vec<String> = reported_associations(name, &router, &table)
            .await
            .into_iter()
            .map(|(attachment_id, _)| attachment_id)
            .collect();
        held.sort();
        let mut expected = holders.clone();
        expected.sort();
        assert_eq!(
            held, expected,
            "case {name:?}: a refused delete must leave every association where it was"
        );
    }

    // The control: the very same delete of a table nothing is associated with succeeds, so
    // the refusals above are about the associations rather than about the operation.
    let free = route_table("a table nothing holds", &router, &gateway).await;
    let body = invoke_ok(
        "a table nothing holds",
        &router,
        "DeleteTransitGatewayRouteTable",
        &[param("TransitGatewayRouteTableId", &free)],
    )
    .await;
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(DELETING),
        "a table nothing holds must be deletable, got {body}"
    );
}

/// The other member of the family, on the same rule: a transit gateway attachments still hold
/// is refused `IncorrectState` naming the gateway the request named and no attachment id,
/// however many attachments hold it — and a gateway nothing holds is deletable.
#[tokio::test]
async fn a_transit_gateway_its_attachments_hold_is_refused_for_its_state() {
    let (router, _engine) = super::ec2_query::emulator();
    let mut minted: u8 = 100;

    for attachments in [1_u8, 2] {
        let name = if attachments == 1 {
            "a gateway one attachment holds"
        } else {
            "a gateway two attachments hold"
        };
        let gateway = transit_gateway(name, &router).await;
        let mut holders: Vec<String> = Vec::new();
        for _ in 0..attachments {
            minted += 1;
            holders.push(attachment(name, &router, &gateway, minted).await);
        }

        let (status, body) = invoke(
            &router,
            "DeleteTransitGateway",
            &[param("TransitGatewayId", &gateway)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {name:?}: a gateway its attachments hold must be refused, got {status} with \
             body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code,
            ec2::state::INCORRECT_STATE,
            "case {name:?}: expected the code a state that forbids the delete carries, got \
             {code:?} in {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            format!("{gateway} has non-deleted Transit Gateway Attachments and cannot be deleted."),
            "case {name:?}: the refusal must name the gateway the request named, got {message:?}"
        );
        for holder in &holders {
            assert!(
                !message.contains(holder.as_str()),
                "case {name:?}: the refusal must name no attachment id, and it names \
                 {holder:?}: {message:?}"
            );
        }

        let (mut still_held, states) = reported_attachments(name, &router, &holders).await;
        still_held.sort();
        let mut expected = holders.clone();
        expected.sort();
        assert_eq!(
            still_held, expected,
            "case {name:?}: a refused delete must leave every attachment where it was"
        );
        assert!(
            !states.iter().any(|state| state == DELETING),
            "case {name:?}: a refused delete must move no attachment towards removal, got \
             {states:?}"
        );
        let state = reported_gateway_state(name, &router, &gateway).await;
        assert_ne!(
            state.as_deref(),
            Some(DELETING),
            "case {name:?}: a refused delete must leave the gateway as it was, got {state:?}"
        );
    }

    // The control, as above: a gateway nothing holds is deletable.
    let free = transit_gateway("a gateway nothing holds", &router).await;
    let body = invoke_ok(
        "a gateway nothing holds",
        &router,
        "DeleteTransitGateway",
        &[param("TransitGatewayId", &free)],
    )
    .await;
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(DELETING),
        "a gateway nothing holds must be deletable, got {body}"
    );
}

/// The remediation the refusal points a client at: a disassociation takes the association
/// down — answered in the transient `disassociating` state, with the record gone once the
/// clock has finished the move — and the delete that was refused while it was there then
/// succeeds. Every way the disassociation can fail to resolve what it was given is refused
/// instead, and writes nothing.
#[tokio::test]
async fn disassociating_an_attachment_makes_its_route_table_deletable_again() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "the association a delete waits for";
    let gateway = transit_gateway(case, &router).await;
    let table = route_table(case, &router, &gateway).await;
    let holder = attachment(case, &router, &gateway, 7).await;
    invoke_ok(
        case,
        &router,
        "AssociateTransitGatewayRouteTable",
        &[
            param("TransitGatewayRouteTableId", &table),
            param("TransitGatewayAttachmentId", &holder),
        ],
    )
    .await;
    poll_until_associations_settled(case, &router, &table, 1).await;

    // Refused while the association is there, so what follows is the association going away
    // rather than a delete that would have worked all along.
    let (status, body) = invoke(
        &router,
        "DeleteTransitGatewayRouteTable",
        &[param("TransitGatewayRouteTableId", &table)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: the delete must be refused while the association is there, got \
         {status} with body {body}"
    );

    // Every way the disassociation can fail to resolve what it was given, asked BEFORE the
    // one that works so a refusal cannot be mistaken for the real disassociation.
    //
    // (case, the request's fields, the HTTP status and the code expected)
    type RefusalCase = (String, Vec<Param>, u16, &'static str);
    let unassociated = attachment("an attachment no route table holds", &router, &gateway, 8).await;
    let refusals: Vec<RefusalCase> = vec![
        (
            "a value that is not a route table id at all".to_string(),
            vec![
                param("TransitGatewayRouteTableId", "tgw-rtb-not-an-id"),
                param("TransitGatewayAttachmentId", &holder),
            ],
            400,
            ec2::state::INVALID_ROUTE_TABLE_ID_MALFORMED,
        ),
        (
            "a value that is not an attachment id at all".to_string(),
            vec![
                param("TransitGatewayRouteTableId", &table),
                param("TransitGatewayAttachmentId", "tgw-attach-not-an-id"),
            ],
            400,
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "a well-formed route table id no create ever minted".to_string(),
            vec![
                param("TransitGatewayRouteTableId", "tgw-rtb-0123456789abcdef0"),
                param("TransitGatewayAttachmentId", &holder),
            ],
            400,
            ec2::state::INVALID_ROUTE_TABLE_ID_NOT_FOUND,
        ),
        (
            "a well-formed attachment id no create ever minted".to_string(),
            vec![
                param("TransitGatewayRouteTableId", &table),
                param("TransitGatewayAttachmentId", "tgw-attach-0123456789abcdef0"),
            ],
            400,
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "a pair the account holds no association for".to_string(),
            vec![
                param("TransitGatewayRouteTableId", &table),
                param("TransitGatewayAttachmentId", &unassociated),
            ],
            400,
            ec2::state::INCORRECT_STATE,
        ),
        (
            "a dry run of a disassociation that would have succeeded".to_string(),
            vec![
                param("TransitGatewayRouteTableId", &table),
                param("TransitGatewayAttachmentId", &holder),
                param("DryRun", "true"),
            ],
            ec2::state::DRY_RUN_OPERATION_STATUS,
            ec2::state::DRY_RUN_OPERATION,
        ),
    ];
    for (name, request, expected_status, expected_code) in &refusals {
        let (status, body) = invoke(&router, "DisassociateTransitGatewayRouteTable", request).await;
        assert_eq!(
            status, *expected_status,
            "case {name:?}: expected HTTP {expected_status}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, *expected_code,
            "case {name:?}: expected the code {expected_code:?}, got {code:?} in {body}"
        );
    }
    // The pair that holds no association is refused for the STATE it is in, naming both ids
    // the request sent.
    let (_, body) = invoke(
        &router,
        "DisassociateTransitGatewayRouteTable",
        &[
            param("TransitGatewayRouteTableId", &table),
            param("TransitGatewayAttachmentId", &unassociated),
        ],
    )
    .await;
    assert_eq!(
        error_message(&body),
        format!("{unassociated} is not associated with {table}"),
        "the refusal must name the pair the request sent, got {body}"
    );
    // None of those refusals touched the association that IS there.
    let held = reported_associations(case, &router, &table).await;
    assert_eq!(
        held,
        vec![(
            holder.clone(),
            TRANSIT_GATEWAY_ASSOCIATION_STATE_ASSOCIATED.to_string()
        )],
        "a refused disassociation must leave the association exactly as it was"
    );

    // The disassociation itself: answered in the TRANSIENT state, naming the pair it took
    // down, and the record is reported in that state until the clock finishes the move.
    let body = invoke_ok(
        case,
        &router,
        "DisassociateTransitGatewayRouteTable",
        &[
            param("TransitGatewayRouteTableId", &table),
            param("TransitGatewayAttachmentId", &holder),
        ],
    )
    .await;
    assert_eq!(
        element(&body, "transitGatewayRouteTableId").as_deref(),
        Some(table.as_str()),
        "the disassociation must answer the table it was asked about, got {body}"
    );
    assert_eq!(
        element(&body, "transitGatewayAttachmentId").as_deref(),
        Some(holder.as_str()),
        "the disassociation must answer the attachment it took down, got {body}"
    );
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(TRANSIT_GATEWAY_ASSOCIATION_STATE_DISASSOCIATING),
        "the disassociation must answer the transient state it put the association in, got \
         {body}"
    );

    // Once the clock has finished the move the association is gone, which is what makes the
    // table deletable — polled for, so nothing here depends on how fast the host runs.
    let deadline = tokio::time::Instant::now() + MOVE_DEADLINE;
    loop {
        let held = reported_associations(case, &router, &table).await;
        if held.is_empty() {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: a disassociated association must stop being reported, and {table} \
             still reports {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let body = invoke_ok(
        case,
        &router,
        "DeleteTransitGatewayRouteTable",
        &[param("TransitGatewayRouteTableId", &table)],
    )
    .await;
    assert_eq!(
        element(&body, "state").as_deref(),
        Some(DELETING),
        "a table whose association has gone must be deletable, got {body}"
    );
}
