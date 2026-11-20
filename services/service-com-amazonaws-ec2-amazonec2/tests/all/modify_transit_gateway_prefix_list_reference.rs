// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyTransitGatewayPrefixListReference`, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol") together with the
//! `CreateTransitGatewayPrefixListReference` that puts the route there, the
//! `GetTransitGatewayPrefixListReferences` that reads it back and the
//! `DeleteTransitGatewayPrefixListReference` that takes it out again.
//!
//! The rule under test: modifying a prefix list reference REPLACES the target of a route
//! that is already in the named transit gateway route table — where the traffic it matches
//! goes, an attachment or the drop a blackhole is — in place, keyed by the (route table,
//! prefix list) pair the request names, so the call answers the STORED record in the
//! non-terminal `modifying` state with the clock owing it `available`, the reader reports
//! exactly what the modify answered, and every other route of the table is left alone. Every
//! id the request names is resolved first, so one of the wrong form is refused as malformed
//! and one that resolves to nothing as that resource's own not-found — a `PrefixListId` that
//! is no route of the named table being a prefix list THIS surface cannot resolve; a request
//! that says
//! neither where matching traffic goes nor that it is dropped, or says both, and one naming
//! a route on its way out of its table, are refused as well — and a refused modify and a dry
//! run write nothing.
//!
//! Each table row is a distinct input along what that rule generalises over: what the route
//! did with matching traffic before the modify and what it does after (every direction
//! between a blackhole and each attachment kind this crate stores, including a move from one
//! attachment to another), which published id form the prefix list carries, and whether the
//! route is still `pending` or already `available` when the modify arrives.

use std::time::Duration;

use ec2::ops::types::{
    PeeringTgwInfo, TransitGatewayClientVpnAttachment, TransitGatewayPeeringAttachment,
    TransitGatewayVpcAttachment,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, PREFIX_LIST_REFERENCE_STATE_AVAILABLE,
    PREFIX_LIST_REFERENCE_STATE_DELETING, PREFIX_LIST_REFERENCE_STATE_MODIFYING,
    PREFIX_LIST_REFERENCE_STATE_PENDING, TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_CLIENT_VPN,
    TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_PEERING, TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPC,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE, TransitGatewayAttachmentView,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message, filters};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// `MissingParameter`, the code EC2 answers for a request that left out a parameter the
/// operation needs (`crate::scaffolded::Scaffolded::missing_parameter`).
const MISSING_PARAMETER: &str = "MissingParameter";

/// `InvalidParameterCombination`, the code EC2 answers for members that cannot hold together
/// (`crate::scaffolded::Scaffolded::invalid_parameter_combination`).
const INVALID_PARAMETER_COMBINATION: &str = "InvalidParameterCombination";

/// Long enough past the span a clock-owed move takes that it is certainly overdue: the
/// deadline the polls below give up at, generous because what is under test is the state a
/// reference reaches rather than how fast it reaches it.
fn move_deadline() -> Duration {
    Duration::from_secs(60)
}

/// How long a poll below waits between two reads of a reference's state.
fn poll_interval() -> Duration {
    Duration::from_millis(50)
}

/// Wait until the reference of `prefix_list_id` in `route_table_id` is reported in `state`.
///
/// Polls for that STATE rather than sleeping for the span the clock owes the move, so a
/// loaded machine cannot read the reference before the move has been applied.
async fn wait_until_reported(
    router: &MultiServiceRouter,
    route_table_id: &str,
    prefix_list_id: &str,
    state: &str,
) {
    let deadline = tokio::time::Instant::now() + move_deadline();
    loop {
        let reported = reported_states(router, route_table_id, prefix_list_id).await;
        if reported == vec![state.to_string()] {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the reference of {prefix_list_id} in {route_table_id} must reach {state:?}, \
             last read {reported:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// One call of `action` signed for the Region above.
async fn invoke(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// A transit gateway created through the wire, as the id of the default ASSOCIATION route
/// table the create owes it ("we create a default transit gateway route table and use it as
/// the default association route table" — `CreateTransitGateway`).
async fn route_table(router: &MultiServiceRouter) -> String {
    let (status, body) = invoke(router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "CreateTransitGateway must succeed, got {status} with body {body}"
    );
    element(&body, "associationDefaultRouteTableId")
        .expect("a gateway created with the documented defaults has an association route table")
}

/// One kind of attachment a route can forward to, with the `ResourceType` and `ResourceId`
/// the reference then reports and the stored attachment it is seeded as.
struct Kind {
    /// The attachment's `TransitGatewayAttachmentResourceType`.
    resource_type: &'static str,
    /// The id of the resource the attachment attaches.
    resource_id: &'static str,
    /// An available attachment of this kind, as it is stored.
    view: fn() -> TransitGatewayAttachmentView,
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

/// A transit gateway peering attachment in service. Its `ResourceId` is the accepter gateway,
/// which is what a report of the attachment names.
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

/// The attachment kinds a route can forward to, one row per `ResourceType` this crate stores.
fn kinds() -> Vec<Kind> {
    vec![
        Kind {
            resource_type: TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPC,
            resource_id: "vpc-0123456789abcdef0",
            view: available_vpc_attachment,
        },
        Kind {
            resource_type: TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_PEERING,
            resource_id: "tgw-00000000000000aaa",
            view: available_peering_attachment,
        },
        Kind {
            resource_type: TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_CLIENT_VPN,
            resource_id: "cvpn-endpoint-0123456789abcdef0",
            view: available_client_vpn_attachment,
        },
    ]
}

/// Store an available attachment of each kind and answer their ids, in the order of
/// [`kinds`], so a row can point a route at any of them.
fn seed_attachments(engine: &engine::InMemoryEngine) -> Vec<String> {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    kinds()
        .iter()
        .map(|kind| {
            guard
                .insert_transit_gateway_attachment((kind.view)())
                .expect("the store holds the seeded attachment")
        })
        .collect()
}

/// Store a managed prefix list under each id the rows below name, so a reference created FOR
/// one names a prefix list the account holds.
///
/// A reference routes exactly the ranges its prefix list holds, so the create resolves the
/// `PrefixListId` against the account's own lists; which list it is has no part in any rule
/// here, the rows being about the route's TARGET.
fn seed_prefix_lists(engine: &engine::InMemoryEngine) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    for prefix_list_id in [
        "pl-00000000000000aa1",
        "pl-0000aa12",
        "pl-00000000000000bb2",
        "pl-0000bb23",
        "pl-00000000000000cc3",
        "pl-0000cc34",
        "pl-00000000000000dd4",
    ] {
        guard.managed_prefix_lists.insert(
            prefix_list_id.to_string(),
            ec2::state::ManagedPrefixListRecord {
                view: ec2::ops::types::ManagedPrefixList {
                    prefix_list_id: Some(prefix_list_id.to_string()),
                    address_family: Some("IPv4".to_string()),
                    state: Some("create-complete".to_string()),
                    prefix_list_name: Some(format!("seeded-{prefix_list_id}")),
                    max_entries: Some(5),
                    version: Some(1),
                    owner_id: Some(engine::emulator_account_id().to_string()),
                    ..Default::default()
                },
                entries: Vec::new(),
                pending: None,
                pending_revision: None,
            },
        );
    }
}

/// The state the store holds for one (route table, prefix list) pair, or `None` when it holds
/// no reference for it at all.
fn stored_state(
    engine: &engine::InMemoryEngine,
    route_table_id: &str,
    prefix_list_id: &str,
) -> Option<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_prefix_list_references
        .get(&(route_table_id.to_string(), prefix_list_id.to_string()))
        .and_then(|record| record.view.state.clone())
}

/// Whether the stored route for one pair drops the traffic it matches, and which attachment it
/// forwards to — the TARGET a modify replaces, read out of the store rather than off a
/// response.
fn stored_target(
    engine: &engine::InMemoryEngine,
    route_table_id: &str,
    prefix_list_id: &str,
) -> Option<(bool, Option<String>)> {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_prefix_list_references
        .get(&(route_table_id.to_string(), prefix_list_id.to_string()))
        .map(|record| {
            (
                record.view.blackhole.unwrap_or(false),
                record
                    .view
                    .transit_gateway_attachment
                    .as_ref()
                    .and_then(|attachment| attachment.transit_gateway_attachment_id.clone()),
            )
        })
}

/// How many bytes of caller-supplied data the store holds: a modify names the pair the record
/// is keyed by rather than changing it, so this figure must not move.
fn stored_bytes(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().stored_bytes()
}

/// The body `GetTransitGatewayPrefixListReferences` answers for `route_table_id`, narrowed to
/// the prefix list `prefix_list_id`.
async fn reported_body(
    router: &MultiServiceRouter,
    route_table_id: &str,
    prefix_list_id: &str,
) -> String {
    let mut params = vec![param("TransitGatewayRouteTableId", route_table_id)];
    params.extend(filters(&[("prefix-list-id", &[prefix_list_id])]));
    let (status, body) = invoke(router, "GetTransitGatewayPrefixListReferences", &params).await;
    assert_eq!(
        status, 200,
        "GetTransitGatewayPrefixListReferences must succeed, got {status} with body {body}"
    );
    body
}

/// The states `GetTransitGatewayPrefixListReferences` reports for one pair.
async fn reported_states(
    router: &MultiServiceRouter,
    route_table_id: &str,
    prefix_list_id: &str,
) -> Vec<String> {
    elements(
        &reported_body(router, route_table_id, prefix_list_id).await,
        "state",
    )
}

/// Where the traffic a route matches goes, which is what the rows below vary on both sides of
/// the modify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    /// `Blackhole`: the traffic is dropped, and the route names no attachment.
    Blackhole,
    /// `TransitGatewayAttachmentId`: the traffic is routed to the seeded attachment at this
    /// index of [`kinds`].
    Attachment(usize),
}

/// The request members that ask for `target`.
fn target_params(target: Target, attachment_ids: &[String]) -> Vec<(String, String)> {
    match target {
        Target::Blackhole => vec![param("Blackhole", "true")],
        Target::Attachment(index) => vec![param(
            "TransitGatewayAttachmentId",
            &attachment_ids[index],
        )],
    }
}

/// One refused modify, as a row of the table below: the case name, the `TransitGatewayRouteTableId`
/// and `PrefixListId` the request names, the members asking for a target, and the code the
/// refusal must carry.
type Refusal<'a> = (&'a str, &'a str, &'a str, Vec<(String, String)>, &'a str);

/// One modify, as a row of the table.
struct Case {
    /// The case name every assertion of the row quotes.
    name: &'static str,
    /// The `PrefixListId` the route is for.
    prefix_list_id: &'static str,
    /// What the route did with matching traffic when it was created.
    before: Target,
    /// What the modify asks it to do with that traffic instead.
    after: Target,
    /// Whether the clock-owed `available` move has been let fall due before the modify, so
    /// the route is modified from `available` rather than from `pending`.
    modify_when_available: bool,
}

/// Create one reference through the wire, pointing at `target`.
async fn create_reference(
    router: &MultiServiceRouter,
    case: &str,
    route_table_id: &str,
    prefix_list_id: &str,
    target: Target,
    attachment_ids: &[String],
) {
    let mut request = vec![
        param("TransitGatewayRouteTableId", route_table_id),
        param("PrefixListId", prefix_list_id),
    ];
    request.extend(target_params(target, attachment_ids));
    let (status, body) = invoke(router, "CreateTransitGatewayPrefixListReference", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the create must succeed, got {status} with body {body}"
    );
}

/// Assert that `body` describes a route pointing at `target`: its blackhole flag and the
/// attachment members of the kind it forwards to, or no attachment at all.
fn assert_describes_target(
    case: &str,
    what: &str,
    body: &str,
    target: Target,
    attachment_ids: &[String],
) {
    assert_eq!(
        element(body, "blackhole").as_deref(),
        Some(if target == Target::Blackhole {
            "true"
        } else {
            "false"
        }),
        "case {case:?}: {what} must carry the route's own blackhole flag, got {body}"
    );
    match target {
        Target::Blackhole => assert_eq!(
            elements(body, "transitGatewayAttachment"),
            Vec::<String>::new(),
            "case {case:?}: {what} must name no attachment for a blackhole route, got {body}"
        ),
        Target::Attachment(index) => {
            let kind = &kinds()[index];
            assert_eq!(
                element(body, "transitGatewayAttachmentId").as_deref(),
                Some(attachment_ids[index].as_str()),
                "case {case:?}: {what} must name the attachment the route forwards to, got \
                 {body}"
            );
            assert_eq!(
                element(body, "resourceType").as_deref(),
                Some(kind.resource_type),
                "case {case:?}: {what} must carry the stored attachment's own resource type, \
                 got {body}"
            );
            assert_eq!(
                element(body, "resourceId").as_deref(),
                Some(kind.resource_id),
                "case {case:?}: {what} must carry the stored attachment's own resource, got \
                 {body}"
            );
        }
    }
}

/// A modify replaces the target of the route it names: the call answers the stored record in
/// `modifying` carrying the NEW target, the reader reports exactly that, the clock then owes
/// the route `available` again, and the pair the record is keyed by — and the bytes charged
/// for it — are untouched.
#[tokio::test]
async fn modifying_a_prefix_list_reference_repoints_the_route_and_reads_it_back() {
    // Every direction between the targets a route can have, both published id forms of an EC2
    // resource id (8 or 17 lowercase hexadecimal digits,
    // `crate::state::prefixed_id_is_well_formed`) and both states the route can be modified
    // from.
    let cases = [
        Case {
            name: "a pending blackhole route pointed at a VPC attachment",
            prefix_list_id: "pl-00000000000000aa1",
            before: Target::Blackhole,
            after: Target::Attachment(0),
            modify_when_available: false,
        },
        Case {
            name: "an available blackhole route pointed at a peering attachment",
            prefix_list_id: "pl-0000aa12",
            before: Target::Blackhole,
            after: Target::Attachment(1),
            modify_when_available: true,
        },
        Case {
            name: "an available route to a VPC attachment turned into a blackhole",
            prefix_list_id: "pl-00000000000000bb2",
            before: Target::Attachment(0),
            after: Target::Blackhole,
            modify_when_available: true,
        },
        Case {
            name: "a pending route moved from a VPC to a Client VPN attachment",
            prefix_list_id: "pl-0000bb23",
            before: Target::Attachment(0),
            after: Target::Attachment(2),
            modify_when_available: false,
        },
        Case {
            name: "an available route to a Client VPN attachment turned into a blackhole",
            prefix_list_id: "pl-00000000000000cc3",
            before: Target::Attachment(2),
            after: Target::Blackhole,
            modify_when_available: true,
        },
        Case {
            name: "a pending blackhole route asked to blackhole again",
            prefix_list_id: "pl-0000cc34",
            before: Target::Blackhole,
            after: Target::Blackhole,
            modify_when_available: false,
        },
    ];

    for case in cases {
        let Case {
            name,
            prefix_list_id,
            before,
            after,
            modify_when_available,
        } = case;
        let (router, engine) = super::ec2_query::emulator();
        seed_prefix_lists(&engine);
        let attachment_ids = seed_attachments(&engine);
        let route_table_id = route_table(&router).await;
        let case = name.to_string();

        create_reference(
            &router,
            &case,
            &route_table_id,
            prefix_list_id,
            before,
            &attachment_ids,
        )
        .await;
        let bytes_after_create = stored_bytes(&engine);
        if modify_when_available {
            wait_until_reported(
                &router,
                &route_table_id,
                prefix_list_id,
                PREFIX_LIST_REFERENCE_STATE_AVAILABLE,
            )
            .await;
        } else {
            assert_eq!(
                stored_state(&engine, &route_table_id, prefix_list_id).as_deref(),
                Some(PREFIX_LIST_REFERENCE_STATE_PENDING),
                "case {case:?}: the route must still be on its way in before the modify"
            );
        }

        let mut request = vec![
            param("TransitGatewayRouteTableId", &route_table_id),
            param("PrefixListId", prefix_list_id),
        ];
        request.extend(target_params(after, &attachment_ids));
        let (status, body) =
            invoke(&router, "ModifyTransitGatewayPrefixListReference", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the modify, got {status} with body {body}"
        );
        assert_eq!(
            element(&body, "prefixListId").as_deref(),
            Some(prefix_list_id),
            "case {case:?}: expected the prefix list the request named, got {body}"
        );
        assert_eq!(
            element(&body, "transitGatewayRouteTableId").as_deref(),
            Some(route_table_id.as_str()),
            "case {case:?}: expected the route table the request named, got {body}"
        );
        assert_eq!(
            element(&body, "prefixListOwnerId").as_deref(),
            Some(engine::emulator_account_id()),
            "case {case:?}: expected the account this emulator serves, got {body}"
        );
        assert_eq!(
            element(&body, "state").as_deref(),
            Some(PREFIX_LIST_REFERENCE_STATE_MODIFYING),
            "case {case:?}: expected the non-terminal state of a modify, got {body}"
        );
        assert_describes_target(
            &case,
            "the modify's answer",
            &body,
            after,
            &attachment_ids,
        );

        // The span is never zero, so a caller reading straight after the modify still sees the
        // route being changed rather than in force — and it already carries the new target.
        assert_eq!(
            stored_state(&engine, &route_table_id, prefix_list_id).as_deref(),
            Some(PREFIX_LIST_REFERENCE_STATE_MODIFYING),
            "case {case:?}: the store must hold what the modify reported"
        );
        let expected_target = match after {
            Target::Blackhole => (true, None),
            Target::Attachment(index) => (false, Some(attachment_ids[index].clone())),
        };
        assert_eq!(
            stored_target(&engine, &route_table_id, prefix_list_id),
            Some(expected_target),
            "case {case:?}: the stored route must point where the modify asked"
        );
        let reported = reported_body(&router, &route_table_id, prefix_list_id).await;
        assert_eq!(
            elements(&reported, "state"),
            vec![PREFIX_LIST_REFERENCE_STATE_MODIFYING.to_string()],
            "case {case:?}: the reader must report the state the modify answered with, got \
             {reported}"
        );
        assert_describes_target(
            &case,
            "the reader's report",
            &reported,
            after,
            &attachment_ids,
        );

        // The `available` the clock owes falls due on its own, without anybody asking for it,
        // and the route keeps the target the modify gave it. Read until it has, rather than
        // after a span of this test's choosing.
        wait_until_reported(
            &router,
            &route_table_id,
            prefix_list_id,
            PREFIX_LIST_REFERENCE_STATE_AVAILABLE,
        )
        .await;
        let reported = reported_body(&router, &route_table_id, prefix_list_id).await;
        assert_describes_target(
            &case,
            "the report of the route once it is in force again",
            &reported,
            after,
            &attachment_ids,
        );
        assert_eq!(
            stored_bytes(&engine),
            bytes_after_create,
            "case {case:?}: a modify names the pair the record is keyed by rather than \
             changing it, so it must charge nothing"
        );
    }
}

/// A modify names ONE route, so the other routes of the same table keep the targets they had
/// — including the same prefix list in ANOTHER route table, which is a different pair.
#[tokio::test]
async fn a_modify_leaves_every_other_route_alone() {
    let (router, engine) = super::ec2_query::emulator();
    seed_prefix_lists(&engine);
    let attachment_ids = seed_attachments(&engine);
    let first_table = route_table(&router).await;
    let second_table = route_table(&router).await;

    let modified = "pl-00000000000000aa1";
    let kept = ["pl-00000000000000bb2", "pl-0000cc34"];
    for prefix_list_id in [modified, kept[0], kept[1]] {
        create_reference(
            &router,
            "a table with three blackhole routes",
            &first_table,
            prefix_list_id,
            Target::Blackhole,
            &attachment_ids,
        )
        .await;
    }
    // The same prefix list in another route table: a different pair, so the modify below must
    // not touch it.
    create_reference(
        &router,
        "the same prefix list in another table",
        &second_table,
        modified,
        Target::Blackhole,
        &attachment_ids,
    )
    .await;

    let mut request = vec![
        param("TransitGatewayRouteTableId", &first_table),
        param("PrefixListId", modified),
    ];
    request.extend(target_params(Target::Attachment(1), &attachment_ids));
    let (status, body) = invoke(&router, "ModifyTransitGatewayPrefixListReference", &request).await;
    assert_eq!(
        status, 200,
        "the modify must succeed, got {status} with body {body}"
    );

    for prefix_list_id in kept {
        assert_eq!(
            stored_target(&engine, &first_table, prefix_list_id),
            Some((true, None)),
            "case {prefix_list_id:?}: another route of the same table must keep its target"
        );
    }
    assert_eq!(
        stored_target(&engine, &second_table, modified),
        Some((true, None)),
        "the same prefix list in another route table is a different pair and must keep its \
         target"
    );
    assert_eq!(
        stored_target(&engine, &first_table, modified),
        Some((false, Some(attachment_ids[1].clone()))),
        "the route the modify named must point where it asked"
    );
}

/// Every id a modify names is resolved before anything is written, each with its own
/// resource's code, and a request that does not say exactly one thing about where matching
/// traffic goes is refused too — none of which writes anything.
#[tokio::test]
async fn a_modify_whose_request_does_not_resolve_is_refused_without_writing() {
    let (router, engine) = super::ec2_query::emulator();
    seed_prefix_lists(&engine);
    let attachment_ids = seed_attachments(&engine);
    let route_table_id = route_table(&router).await;
    let referenced = "pl-00000000000000aa1";
    create_reference(
        &router,
        "the route the refusals must not touch",
        &route_table_id,
        referenced,
        Target::Blackhole,
        &attachment_ids,
    )
    .await;

    let cases: Vec<Refusal<'_>> = vec![
        (
            "a route table id of the right form that names no route table",
            "tgw-rtb-0000000000000000f",
            referenced,
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_ROUTE_TABLE_ID_NOT_FOUND,
        ),
        (
            "a short-form route table id that names no route table",
            "tgw-rtb-0000000f",
            referenced,
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_ROUTE_TABLE_ID_NOT_FOUND,
        ),
        (
            "a route table id that is not an id of that kind",
            "tgw-rtb-0123456789abcde",
            referenced,
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_ROUTE_TABLE_ID_MALFORMED,
        ),
        (
            "an attachment id where a route table id belongs",
            &attachment_ids[0],
            referenced,
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_ROUTE_TABLE_ID_MALFORMED,
        ),
        (
            "a prefix list id that is not an id of that kind",
            &route_table_id,
            "pl-not-an-id",
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_PREFIX_LIST_ID_MALFORMED,
        ),
        (
            "a prefix list id carrying a digit outside the id alphabet",
            &route_table_id,
            "pl-0000000g",
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_PREFIX_LIST_ID_MALFORMED,
        ),
        (
            "a prefix list the named route table does not route",
            &route_table_id,
            "pl-00000000000000dd4",
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
        ),
        (
            "a short-form prefix list the named route table does not route",
            &route_table_id,
            "pl-0000bb23",
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
        ),
        (
            "a prefix list id no list of the account or of the Region has",
            &route_table_id,
            "pl-00000000000000ee5",
            vec![param("Blackhole", "true")],
            ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND,
        ),
        (
            "an attachment id that is not an id of that kind",
            &route_table_id,
            referenced,
            vec![param("TransitGatewayAttachmentId", "tgw-attach-not-an-id")],
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_MALFORMED,
        ),
        (
            "an attachment id of the right form that names no attachment",
            &route_table_id,
            referenced,
            vec![param(
                "TransitGatewayAttachmentId",
                "tgw-attach-0000000000000000f",
            )],
            ec2::state::INVALID_TRANSIT_GATEWAY_ATTACHMENT_ID_NOT_FOUND,
        ),
        (
            "nothing said about where matching traffic goes",
            &route_table_id,
            referenced,
            Vec::new(),
            MISSING_PARAMETER,
        ),
        (
            "a blackhole asked for explicitly alongside no attachment",
            &route_table_id,
            referenced,
            vec![param("Blackhole", "false")],
            MISSING_PARAMETER,
        ),
        (
            "both a blackhole and an attachment to forward to",
            &route_table_id,
            referenced,
            vec![
                param("Blackhole", "true"),
                param("TransitGatewayAttachmentId", &attachment_ids[0]),
            ],
            INVALID_PARAMETER_COMBINATION,
        ),
    ];

    for (name, table_id, prefix_list_id, target, expected) in cases {
        let case = format!("ModifyTransitGatewayPrefixListReference with {name}");
        let mut request = vec![
            param("TransitGatewayRouteTableId", table_id),
            param("PrefixListId", prefix_list_id),
        ];
        request.extend(target);
        let (status, body) =
            invoke(&router, "ModifyTransitGatewayPrefixListReference", &request).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected,
            "case {case:?}: expected {expected}, got {body}"
        );
        if expected == ec2::state::INVALID_PREFIX_LIST_ID_NOT_FOUND {
            // The sentence EC2 words that code with for this surface, built from the two ids
            // the request named (`crate::state::transit_gateway_prefix_list_not_referenced`).
            assert_eq!(
                error_message(&body),
                format!(
                    "PrefixListId {prefix_list_id} is not referenced in Transit Gateway Route \
                     Table {table_id}."
                ),
                "case {case:?}: expected the surface's own sentence for {expected}, got {body}"
            );
        }
        assert_eq!(
            stored_target(&engine, &route_table_id, referenced),
            Some((true, None)),
            "case {case:?}: a refused modify must write nothing"
        );
        assert_eq!(
            stored_state(&engine, &route_table_id, referenced).as_deref(),
            Some(PREFIX_LIST_REFERENCE_STATE_PENDING),
            "case {case:?}: a refused modify must leave the route's state where it was"
        );
    }
}

/// A dry run of a modify that WOULD have succeeded performs nothing: the route still points
/// where it did, in the state it was in, and the call that follows is the one that repoints
/// it.
#[tokio::test]
async fn a_dry_run_modifies_no_reference() {
    // Both targets a dry run can ask for, so it is answered the same way whichever one the
    // request names.
    for (case, prefix_list_id, target) in [
        (
            "a dry run asking for an attachment",
            "pl-00000000000000aa1",
            Target::Attachment(0),
        ),
        (
            "a dry run asking for a blackhole",
            "pl-00000000000000bb2",
            Target::Blackhole,
        ),
    ] {
        let (router, engine) = super::ec2_query::emulator();
        seed_prefix_lists(&engine);
        let attachment_ids = seed_attachments(&engine);
        let route_table_id = route_table(&router).await;
        let before = match target {
            Target::Blackhole => Target::Attachment(1),
            Target::Attachment(_) => Target::Blackhole,
        };
        create_reference(
            &router,
            case,
            &route_table_id,
            prefix_list_id,
            before,
            &attachment_ids,
        )
        .await;
        let target_before = stored_target(&engine, &route_table_id, prefix_list_id);

        let mut request = vec![
            param("TransitGatewayRouteTableId", &route_table_id),
            param("PrefixListId", prefix_list_id),
        ];
        request.extend(target_params(target, &attachment_ids));
        let dry_run = {
            let mut dry_run = request.clone();
            dry_run.push(param("DryRun", "true"));
            dry_run
        };
        let (status, body) =
            invoke(&router, "ModifyTransitGatewayPrefixListReference", &dry_run).await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            stored_target(&engine, &route_table_id, prefix_list_id),
            target_before,
            "case {case:?}: a dry run must repoint nothing"
        );

        let (status, body) =
            invoke(&router, "ModifyTransitGatewayPrefixListReference", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 after a dry run, got {status} with body {body}"
        );
        assert_eq!(
            stored_state(&engine, &route_table_id, prefix_list_id).as_deref(),
            Some(PREFIX_LIST_REFERENCE_STATE_MODIFYING),
            "case {case:?}: the call after the dry run is the one that repoints the route"
        );
    }
}

/// A route on its way out of its table is not one there is anything left to repoint: the
/// modify is refused for the state the reference is in, and the removal the delete started
/// still carries it out.
#[tokio::test]
async fn a_modify_of_a_route_on_its_way_out_is_refused_for_its_state() {
    let (router, engine) = super::ec2_query::emulator();
    seed_prefix_lists(&engine);
    let attachment_ids = seed_attachments(&engine);
    let route_table_id = route_table(&router).await;
    let prefix_list_id = "pl-00000000000000aa1";
    create_reference(
        &router,
        "a route deleted and then modified",
        &route_table_id,
        prefix_list_id,
        Target::Blackhole,
        &attachment_ids,
    )
    .await;

    let pair = vec![
        param("TransitGatewayRouteTableId", &route_table_id),
        param("PrefixListId", prefix_list_id),
    ];
    let (status, body) = invoke(&router, "DeleteTransitGatewayPrefixListReference", &pair).await;
    assert_eq!(
        status, 200,
        "the delete must succeed, got {status} with body {body}"
    );
    assert_eq!(
        stored_state(&engine, &route_table_id, prefix_list_id).as_deref(),
        Some(PREFIX_LIST_REFERENCE_STATE_DELETING),
        "the delete must leave the route on its way out"
    );

    let mut request = pair.clone();
    request.extend(target_params(Target::Attachment(0), &attachment_ids));
    let (status, body) = invoke(&router, "ModifyTransitGatewayPrefixListReference", &request).await;
    assert_eq!(
        status, 400,
        "a route on its way out cannot be repointed, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        ec2::state::INCORRECT_STATE,
        "expected {}, got {body}",
        ec2::state::INCORRECT_STATE
    );
    assert_eq!(
        stored_target(&engine, &route_table_id, prefix_list_id),
        Some((true, None)),
        "the refused modify must write nothing"
    );
    assert_eq!(
        stored_state(&engine, &route_table_id, prefix_list_id).as_deref(),
        Some(PREFIX_LIST_REFERENCE_STATE_DELETING),
        "the refused modify must leave the route on its way out"
    );
}
