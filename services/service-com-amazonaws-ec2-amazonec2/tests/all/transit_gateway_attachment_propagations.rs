// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetTransitGatewayAttachmentPropagations`, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: the operation reports, for the attachment the request names, one
//! entry per propagation route table that attachment's routes propagate into — the route
//! table's id and the propagation's state as the store holds them — narrowed by the one
//! documented filter and paged in route-table-id order, and the attachment id is resolved
//! against the store first, so an id of the wrong form is refused as malformed and one that
//! resolves to no attachment as not found rather than reported as "propagates into nothing".
//!
//! Each table row is a distinct input along what that rule generalises over: how many route
//! tables the attachment propagates into (none, one, several), which kind of attachment it is
//! (every kind shares one id space and every kind can propagate), which value the documented
//! filter carries, what page size the request asks for, and which way the attachment id fails
//! to resolve.

use std::time::{Duration, Instant};

use ec2::ops::types::{
    PeeringTgwInfo, TransitGatewayClientVpnAttachment, TransitGatewayPeeringAttachment,
    TransitGatewayVpcAttachment,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, PROPAGATION_STATE_ENABLED,
    PROPAGATION_STATE_ENABLING, TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE,
    TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, filters};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// How long a poll for a clock-owed move waits before it gives up: far longer than the span
/// the service owes such a move, so a loaded or slow host does not turn a due transition into
/// a failure. Nothing here asserts on how long the wait actually took.
const CLOCK_DEADLINE: Duration = Duration::from_secs(60);

/// How long one poll waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One call of `action` signed for the Region above.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// A transit gateway created through the wire, as the id of the default PROPAGATION route
/// table the create owes it ("we create a default transit gateway route table and use it as
/// … the default propagation route table" — `CreateTransitGateway`).
async fn propagation_route_table(router: &MultiServiceRouter) -> String {
    let (status, body) = invoke(router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "CreateTransitGateway must succeed, got {status} with body {body}"
    );
    element(&body, "propagationDefaultRouteTableId")
        .expect("a gateway created with the documented defaults has a propagation route table")
}

/// A VPC attachment in service.
fn available_vpc_attachment() -> TransitGatewayAttachmentView {
    TransitGatewayAttachmentView::Vpc(TransitGatewayVpcAttachment {
        transit_gateway_id: Some("tgw-0123456789abcdef0".to_string()),
        vpc_id: Some("vpc-0123456789abcdef0".to_string()),
        vpc_owner_id: Some("210987654321".to_string()),
        state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
        subnet_ids: Some(vec!["subnet-0123456789abcdef0".to_string()]),
        ..Default::default()
    })
}

/// A transit gateway peering attachment in service.
fn available_peering_attachment() -> TransitGatewayAttachmentView {
    TransitGatewayAttachmentView::Peering(TransitGatewayPeeringAttachment {
        requester_tgw_info: Some(PeeringTgwInfo {
            transit_gateway_id: Some("tgw-0123456789abcdef0".to_string()),
            owner_id: Some("210987654321".to_string()),
            region: Some(REGION.to_string()),
            ..Default::default()
        }),
        accepter_tgw_info: Some(PeeringTgwInfo {
            transit_gateway_id: Some("tgw-00000000000000aaa".to_string()),
            owner_id: Some("210987654321".to_string()),
            region: Some("us-west-2".to_string()),
            ..Default::default()
        }),
        state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
        ..Default::default()
    })
}

/// A Client VPN attachment in service.
fn available_client_vpn_attachment() -> TransitGatewayAttachmentView {
    TransitGatewayAttachmentView::ClientVpn(TransitGatewayClientVpnAttachment {
        transit_gateway_id: Some("tgw-0123456789abcdef0".to_string()),
        client_vpn_endpoint_id: Some("cvpn-endpoint-0123456789abcdef0".to_string()),
        client_vpn_owner_id: Some("210987654321".to_string()),
        state: Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE.to_string()),
        ..Default::default()
    })
}

/// One row of the table: a kind of attachment and how many propagation route tables its
/// routes propagate into.
struct Case {
    /// The case name every assertion of the row quotes.
    name: &'static str,
    /// An available attachment of this kind, as it is stored.
    view: fn() -> TransitGatewayAttachmentView,
    /// How many propagation route tables the attachment propagates into.
    tables: usize,
}

/// The rows: every attachment kind this crate stores, each at a different number of
/// propagation route tables — none, one, and more than one page's worth below.
fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "a VPC attachment propagating into no route table",
            view: available_vpc_attachment,
            tables: 0,
        },
        Case {
            name: "a VPC attachment propagating into one route table",
            view: available_vpc_attachment,
            tables: 1,
        },
        Case {
            name: "a peering attachment propagating into three route tables",
            view: available_peering_attachment,
            tables: 3,
        },
        Case {
            name: "a Client VPN attachment propagating into seven route tables",
            view: available_client_vpn_attachment,
            tables: 7,
        },
    ]
}

/// Store an available attachment and answer its `TransitGatewayAttachmentId`.
fn seed_attachment(
    engine: &engine::InMemoryEngine,
    view: fn() -> TransitGatewayAttachmentView,
) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .insert_transit_gateway_attachment(view())
        .expect("the store holds the seeded attachment")
}

/// Enable the propagation of `attachment_id`'s routes into `route_table_id`.
async fn enable(router: &MultiServiceRouter, case: &str, route_table_id: &str, attachment_id: &str) {
    let (status, body) = invoke(
        router,
        "EnableTransitGatewayRouteTablePropagation",
        &[
            param("TransitGatewayRouteTableId", route_table_id),
            param("TransitGatewayAttachmentId", attachment_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the enable must succeed, got {status} with body {body}"
    );
}

/// The (route table id, state) pairs `GetTransitGatewayAttachmentPropagations` reports for
/// `attachment_id`, in the order the response carries them.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    attachment_id: &str,
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut params = vec![param("TransitGatewayAttachmentId", attachment_id)];
    params.extend_from_slice(extra);
    let (status, body) = invoke(router, "GetTransitGatewayAttachmentPropagations", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetTransitGatewayAttachmentPropagations must succeed, got {status} \
         with body {body}"
    );
    let tables = elements(&body, "transitGatewayRouteTableId");
    let states = elements(&body, "state");
    assert_eq!(
        tables.len(),
        states.len(),
        "case {case:?}: every reported propagation carries both members, got {tables:?} and \
         {states:?} in {body}"
    );
    tables.into_iter().zip(states).collect()
}

/// The report once every propagation of `attachment_id` reaches `PROPAGATION_STATE_ENABLED`,
/// which the clock owes each of them on its own.
///
/// Polls for that CONDITION rather than waiting a fixed time, so the row holds on a host of
/// any speed; [`CLOCK_DEADLINE`] only bounds how long a move that never comes is waited for.
async fn reported_once_enabled(
    router: &MultiServiceRouter,
    case: &str,
    attachment_id: &str,
) -> Vec<(String, String)> {
    let give_up_at = Instant::now() + CLOCK_DEADLINE;
    loop {
        let report = reported(router, case, attachment_id, &[]).await;
        if report
            .iter()
            .all(|(_, state)| state == PROPAGATION_STATE_ENABLED)
        {
            return report;
        }
        assert!(
            Instant::now() < give_up_at,
            "case {case:?}: the clock owed every propagation its enabled state, still \
             {report:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Every route table an attachment's routes propagate into is reported once, with the state
/// the store holds for that pair, in route-table-id order — and no propagation of ANOTHER
/// attachment is ever reported under this one.
#[tokio::test]
async fn every_route_table_an_attachment_propagates_into_is_reported_with_its_state() {
    for case in cases() {
        let name = case.name;
        let (router, engine) = super::ec2_query::emulator();
        let attachment_id = seed_attachment(&engine, case.view);

        // A second attachment propagating into a route table of its own: its propagation
        // belongs to no report of the attachment under test.
        let elsewhere_table = propagation_route_table(&router).await;
        let elsewhere = seed_attachment(&engine, available_vpc_attachment);
        enable(&router, name, &elsewhere_table, &elsewhere).await;

        let mut expected: Vec<String> = Vec::new();
        for index in 0..case.tables {
            let route_table_id = propagation_route_table(&router).await;
            enable(&router, name, &route_table_id, &attachment_id).await;
            assert!(
                !expected.contains(&route_table_id),
                "case {name:?}: route table {index} must be a fresh one, got \
                 {route_table_id:?} twice"
            );
            expected.push(route_table_id);
        }
        expected.sort();

        // Straight after the enables every pair is still in the transient state of an enable:
        // the span is never zero, so the report shows the move in progress.
        let report = reported(&router, name, &attachment_id, &[]).await;
        assert_eq!(
            report.iter().map(|(table, _)| table.clone()).collect::<Vec<_>>(),
            expected,
            "case {name:?}: expected every route table once in id order, got {report:?}"
        );
        for (table, state) in &report {
            assert_eq!(
                state, PROPAGATION_STATE_ENABLING,
                "case {name:?}: the propagation into {table} was just enabled, got {state:?}"
            );
        }
        assert!(
            !report.iter().any(|(table, _)| table == &elsewhere_table),
            "case {name:?}: another attachment's propagation must never be reported here, \
             got {report:?}"
        );

        // The move the clock owes each pair falls due on its own, without anybody asking.
        let enabled = reported_once_enabled(&router, name, &attachment_id).await;
        assert_eq!(
            enabled
                .iter()
                .map(|(table, _)| table.clone())
                .collect::<Vec<_>>(),
            expected,
            "case {name:?}: the same route tables are reported once enabled, got {enabled:?}"
        );

        // Disabling one of them takes it out of the report once the removal falls due, which
        // is how the writer's effect is observable through this reader.
        if let Some(dropped) = expected.first().cloned() {
            let (status, body) = invoke(
                &router,
                "DisableTransitGatewayRouteTablePropagation",
                &[
                    param("TransitGatewayRouteTableId", &dropped),
                    param("TransitGatewayAttachmentId", &attachment_id),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {name:?}: the disable must succeed, got {status} with body {body}"
            );
            let give_up_at = Instant::now() + CLOCK_DEADLINE;
            loop {
                let report = reported(&router, name, &attachment_id, &[]).await;
                let tables: Vec<String> =
                    report.iter().map(|(table, _)| table.clone()).collect();
                if !tables.contains(&dropped) {
                    assert_eq!(
                        tables,
                        expected[1..].to_vec(),
                        "case {name:?}: every other route table is still reported, got \
                         {report:?}"
                    );
                    break;
                }
                assert!(
                    Instant::now() < give_up_at,
                    "case {name:?}: the clock owed the disabled propagation its removal, \
                     still {report:?}"
                );
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
    }
}

/// The documented filter narrows the report to the route tables it names, and paging reports
/// every propagation once in route-table-id order whatever page size is asked for.
#[tokio::test]
async fn the_report_narrows_by_the_documented_filter_and_pages_in_route_table_id_order() {
    let case = "an attachment propagating into seven route tables";
    let (router, engine) = super::ec2_query::emulator();
    let attachment_id = seed_attachment(&engine, available_vpc_attachment);
    let mut propagating: Vec<String> = Vec::new();
    for _ in 0..7 {
        let route_table_id = propagation_route_table(&router).await;
        enable(&router, case, &route_table_id, &attachment_id).await;
        propagating.push(route_table_id);
    }
    propagating.sort();
    // A route table of the right form that nothing propagates into: a filter naming it
    // selects nothing rather than everything.
    let unused_table = "tgw-rtb-0000000000000000f";

    // The one filter name `GetTransitGatewayAttachmentPropagationsRequest$Filters` documents,
    // over values that select all, one, and none of the propagations.
    // (case, the filter's values, the route tables expected)
    let filter_cases: Vec<(&str, Vec<String>, Vec<String>)> = vec![
        (
            "no filter at all",
            Vec::new(),
            propagating.clone(),
        ),
        (
            "the filter naming one route table that is propagated into",
            vec![propagating[2].clone()],
            vec![propagating[2].clone()],
        ),
        (
            "the filter naming two route tables that are propagated into",
            vec![propagating[0].clone(), propagating[4].clone()],
            vec![propagating[0].clone(), propagating[4].clone()],
        ),
        (
            "the filter naming a route table nothing propagates into",
            vec![unused_table.to_string()],
            Vec::new(),
        ),
        (
            "the filter naming every route table by a wildcard",
            vec!["tgw-rtb-*".to_string()],
            propagating.clone(),
        ),
    ];

    for (row, values, expected) in filter_cases {
        let extra = if values.is_empty() {
            Vec::new()
        } else {
            let borrowed: Vec<&str> = values.iter().map(String::as_str).collect();
            filters(&[("transit-gateway-route-table-id", borrowed.as_slice())])
        };
        let report = reported(&router, row, &attachment_id, &extra).await;
        let tables: Vec<String> = report.iter().map(|(table, _)| table.clone()).collect();
        assert_eq!(
            tables, expected,
            "case {row:?}: expected {expected:?}, got {report:?}"
        );
    }

    // Paging: a `NextToken` exactly when a propagation follows the page, and following it
    // reports the rest once each, in route-table-id order. The sizes are the smallest the
    // model publishes (`TransitGatewayMaxResults`, range 5..1000), one above it, and exactly
    // the number stored — the boundary at which no token is owed.
    for page_size in [5usize, 6, 7] {
        let row = format!("a page size of {page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut params = vec![
                param("TransitGatewayAttachmentId", &attachment_id),
                param("MaxResults", &page_size.to_string()),
            ];
            if let Some(next) = token.as_deref() {
                params.push(param("NextToken", next));
            }
            let (status, body) =
                invoke(&router, "GetTransitGatewayAttachmentPropagations", &params).await;
            assert_eq!(
                status, 200,
                "case {row:?}: expected HTTP 200, got {status} with body {body}"
            );
            let page = elements(&body, "transitGatewayRouteTableId");
            assert!(
                page.len() <= page_size,
                "case {row:?}: a page must hold at most {page_size} propagations, got {}",
                page.len()
            );
            seen.extend(page);
            token = element(&body, "nextToken");
            assert!(
                seen.len() <= propagating.len(),
                "case {row:?}: paging must not report a propagation twice, got {seen:?}"
            );
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen, propagating,
            "case {row:?}: expected every propagation once in route-table-id order, got {seen:?}"
        );
    }

    // A pagination token this service did not mint names no position in the report.
    let (status, body) = invoke(
        &router,
        "GetTransitGatewayAttachmentPropagations",
        &[
            param("TransitGatewayAttachmentId", &attachment_id),
            param("NextToken", "not-a-token"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "a bogus pagination token must be refused, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::INVALID_PAGINATION_TOKEN,
        "expected InvalidPaginationToken, got {body}"
    );

    // A filter name the operation does not document is refused rather than read as "no
    // results": the rows are the names ANOTHER propagation report documents, which this one
    // does not.
    for undocumented in ["resource-id", "resource-type", "transit-gateway-attachment-id"] {
        let mut params = vec![param("TransitGatewayAttachmentId", &attachment_id)];
        params.extend(filters(&[(undocumented, &["anything"])]));
        let (status, body) =
            invoke(&router, "GetTransitGatewayAttachmentPropagations", &params).await;
        assert_eq!(
            status, 400,
            "case {undocumented:?}: an undocumented filter must be refused, got {status} \
             with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::INVALID_PARAMETER_VALUE,
            "case {undocumented:?}: expected InvalidParameterValue, got {body}"
        );
    }
}

/// The attachment id is resolved against the store, so an id of the wrong form is refused as
/// malformed and an id that resolves to no attachment as not found — never as an empty
/// report, and never as a 501.
#[tokio::test]
async fn an_attachment_id_that_resolves_to_nothing_is_refused_with_its_own_code() {
    let (router, engine) = super::ec2_query::emulator();
    // An attachment the account DOES hold, to pin that the refusals below are about the id
    // and not about the operation.
    let attachment_id = seed_attachment(&engine, available_vpc_attachment);
    let report = reported(&router, "an attachment of the account", &attachment_id, &[]).await;
    assert!(
        report.is_empty(),
        "an attachment that propagates nowhere is reported with no propagations, got \
         {report:?}"
    );

    // (case, the id, the code expected)
    for (row, id, expected) in [
        (
            "an attachment id of the right form that names no attachment",
            "tgw-attach-0000000000000000f",
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "another well-formed attachment id of the account's id space",
            "tgw-attach-0a1b2c3d4e5f60718",
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "an id that is not an attachment id at all",
            "tgw-attach-not-an-id",
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "an id of another resource of this service",
            "tgw-rtb-0123456789abcdef0",
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
        ),
    ] {
        let (status, body) = invoke(
            &router,
            "GetTransitGatewayAttachmentPropagations",
            &[param("TransitGatewayAttachmentId", id)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {row:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {row:?}: expected {expected}, got {body}"
        );
        if expected == ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND {
            assert_eq!(
                error_message(&body),
                format!(
                    "{resource} {id} was deleted or does not exist.",
                    resource = ec2::state::TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_NAME,
                ),
                "case {row:?}: the refusal names the attachment the request asked about, \
                 got {body}"
            );
        }
    }
}

/// A dry run reports that outcome and reads nothing: the report is the same before and after
/// it, and the call that follows is the one that answers.
#[tokio::test]
async fn a_dry_run_reports_the_dry_run_outcome_and_reads_nothing() {
    let case = "a dry run";
    let (router, engine) = super::ec2_query::emulator();
    let attachment_id = seed_attachment(&engine, available_peering_attachment);
    let route_table_id = propagation_route_table(&router).await;
    enable(&router, case, &route_table_id, &attachment_id).await;
    let before = reported(&router, case, &attachment_id, &[]).await;

    let (status, body) = invoke(
        &router,
        "GetTransitGatewayAttachmentPropagations",
        &[
            param("TransitGatewayAttachmentId", &attachment_id),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "case {case:?}: expected DryRunOperation, got {body}"
    );
    assert_eq!(
        reported(&router, case, &attachment_id, &[]).await,
        before,
        "case {case:?}: a dry run reads nothing and changes nothing"
    );
}
