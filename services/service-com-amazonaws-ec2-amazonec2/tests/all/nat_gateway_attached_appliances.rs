// SPDX-License-Identifier: CC0-1.0

//! What `DescribeNatGateways` reports about the APPLIANCES attached to a NAT gateway, driven
//! over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries
//! `Action=DescribeNatGateways`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test: a read of a NAT gateway REPORTS "the proxy appliances attached to the
//! NAT Gateway for filtering and inspecting traffic" (`NatGateway$AttachedAppliances`, the
//! `attachedApplianceSet` of `NatGatewayAttachedApplianceList`) as a PRESENT list for every
//! gateway it reports — empty when the gateway holds no appliance, one entry per appliance
//! otherwise — rather than leaving the member out. An absent member says the appliances were
//! not reported, which is a different answer from "none are attached": a client reading the
//! set to decide whether traffic is being inspected can act on the second and not on the
//! first.
//!
//! The rows differ along what the rule generalises over — how the gateway got into the
//! account (minted by `CreateNatGateway`, public with one and with two Elastic IP addresses,
//! private, and written straight into the store), how many appliances it holds (none at all,
//! one, and one per member of the modelled attachment-state catalog), which of an appliance's
//! optional members are filled, and whether the report is the whole store or one narrowed to
//! a single id — so no row is the specification.
//!
//! A gateway holding appliances is seeded straight into the store: nothing in the EC2 surface
//! attaches a proxy appliance to a NAT gateway (that is Network Firewall's `CreateProxy`), so
//! there is no API path in this emulator that fills the list. Everything else is real: the VPC
//! and subnet come from `CreateVpc`/`CreateSubnet`, the Elastic IP addresses from
//! `AllocateAddress`, and the gateways of the first rows from `CreateNatGateway`.

use std::time::SystemTime;

use ec2::ops::types::{
    NAT_GATEWAY_APPLIANCE_MODIFY_STATE_VALUES, NAT_GATEWAY_APPLIANCE_STATE_VALUES,
    NAT_GATEWAY_APPLIANCE_TYPE_VALUES, NatGateway, NatGatewayAttachedAppliance,
};
use ec2::state::{
    AVAILABILITY_MODE_ZONAL, CONNECTIVITY_TYPE_PRIVATE, CONNECTIVITY_TYPE_PUBLIC,
    ComAmazonawsEc2Amazonec2State as Ec2State, NAT_GATEWAY_ID_PREFIX, NAT_GATEWAY_STATE_AVAILABLE,
    system_time_to_date_time,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The one member of the modelled `NatGatewayApplianceType` catalog
/// (`ec2::ops::types::NAT_GATEWAY_APPLIANCE_TYPE_VALUES`): "for network firewall proxy
/// functionality, this will be `network-firewall-proxy`"
/// (`NatGatewayAttachedAppliance$Type`).
fn appliance_type() -> &'static str {
    NAT_GATEWAY_APPLIANCE_TYPE_VALUES[0]
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// A VPC of `cidr_block` with one subnet of `subnet_cidr` in it, both created over the wire,
/// as that subnet's id.
async fn subnet(
    case: &str,
    router: &MultiServiceRouter,
    cidr_block: &str,
    subnet_cidr: &str,
) -> String {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", cidr_block)]).await;
    let vpc_id = member(case, &body, "vpcId");
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", subnet_cidr),
            param("AvailabilityZone", "us-east-1a"),
        ],
    )
    .await;
    member(case, &body, "subnetId")
}

/// One Elastic IP address out of Amazon's pool, as its allocation id.
async fn allocation(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "AllocateAddress", &[]).await;
    member(case, &body, "allocationId")
}

/// The members one reported appliance carries, each `None` when the appliance reports no such
/// member at all — the shape both the wire and the stored record are read into, so an
/// expectation compares like with like.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Appliance {
    appliance_type: Option<String>,
    appliance_arn: Option<String>,
    vpc_endpoint_id: Option<String>,
    attachment_state: Option<String>,
    modification_state: Option<String>,
    failure_code: Option<String>,
    failure_message: Option<String>,
}

impl Appliance {
    /// The appliance one `<item>` of an `attachedApplianceSet` reports.
    fn from_item(item: roxmltree::Node<'_, '_>) -> Self {
        Self {
            appliance_type: child_text(item, "type"),
            appliance_arn: child_text(item, "applianceArn"),
            vpc_endpoint_id: child_text(item, "vpcEndpointId"),
            attachment_state: child_text(item, "attachmentState"),
            modification_state: child_text(item, "modificationState"),
            failure_code: child_text(item, "failureCode"),
            failure_message: child_text(item, "failureMessage"),
        }
    }

    /// The appliance a stored record holds, as a report of it must come back.
    fn from_record(record: &NatGatewayAttachedAppliance) -> Self {
        Self {
            appliance_type: record.r#type.clone(),
            appliance_arn: record.appliance_arn.clone(),
            vpc_endpoint_id: record.vpc_endpoint_id.clone(),
            attachment_state: record.attachment_state.clone(),
            modification_state: record.modification_state.clone(),
            failure_code: record.failure_code.clone(),
            failure_message: record.failure_message.clone(),
        }
    }
}

/// Whether `attachment_state` is one of the modelled catalog's FAILED members — the states an
/// appliance carries a failure for: "the failure code if the appliance attachment or
/// modification operation failed" (`NatGatewayAttachedAppliance$FailureCode`). Which is what
/// makes some seeded appliances carry every member and others only some.
fn is_failed(attachment_state: &str) -> bool {
    attachment_state.ends_with("-failed")
}

/// One appliance to attach to a seeded gateway: in `attachment_state`, the `index`-th of its
/// gateway, with a failure reported only for a failed state.
fn appliance(index: usize, attachment_state: &str) -> NatGatewayAttachedAppliance {
    let failed = is_failed(attachment_state);
    NatGatewayAttachedAppliance {
        r#type: Some(appliance_type().to_string()),
        appliance_arn: Some(engine::emulator_arn(
            "network-firewall",
            REGION,
            &format!("firewall/inspection-{index}"),
        )),
        vpc_endpoint_id: Some(format!("vpce-0123456789abcde{index}")),
        attachment_state: Some(attachment_state.to_string()),
        modification_state: Some(
            NAT_GATEWAY_APPLIANCE_MODIFY_STATE_VALUES
                [index % NAT_GATEWAY_APPLIANCE_MODIFY_STATE_VALUES.len()]
            .to_string(),
        ),
        failure_code: failed.then(|| "ProxyAttachmentFailed".to_string()),
        failure_message: failed.then(|| {
            format!("The appliance could not reach the state {attachment_state}")
        }),
    }
}

/// One row of the table: the gateway under test and the appliances a report of it must carry.
#[derive(Debug, Clone)]
struct Row {
    case: String,
    nat_gateway_id: String,
    expected: Vec<Appliance>,
}

/// Write one gateway holding `appliances` into the store — `None` for a record that carries no
/// appliance list at all — and answer the row that asserts against it.
fn seed(
    engine: &engine::InMemoryEngine,
    case: &str,
    subnet_id: &str,
    appliances: Option<Vec<NatGatewayAttachedAppliance>>,
) -> Row {
    let expected = appliances
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(Appliance::from_record)
        .collect();
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let nat_gateway_id = guard.next_id(NAT_GATEWAY_ID_PREFIX);
    guard.insert_nat_gateway(NatGateway {
        nat_gateway_id: Some(nat_gateway_id.clone()),
        state: Some(NAT_GATEWAY_STATE_AVAILABLE.to_string()),
        connectivity_type: Some(CONNECTIVITY_TYPE_PUBLIC.to_string()),
        availability_mode: Some(AVAILABILITY_MODE_ZONAL.to_string()),
        subnet_id: Some(subnet_id.to_string()),
        create_time: Some(system_time_to_date_time(SystemTime::now())),
        nat_gateway_addresses: Some(Vec::new()),
        tags: Some(Vec::new()),
        attached_appliances: appliances,
        ..NatGateway::default()
    });
    Row {
        case: case.to_string(),
        nat_gateway_id,
        expected,
    }
}

/// One `CreateNatGateway` carrying `params`, as the id of the gateway it minted.
async fn create_gateway(case: &str, router: &MultiServiceRouter, params: &[Param]) -> String {
    let body = ok(case, router, "CreateNatGateway", params).await;
    member(case, &body, "natGatewayId")
}

/// The appliances a report carries for `nat_gateway_id`: `None` when that gateway's item
/// carries NO `attachedApplianceSet` member at all, and the entries it reported otherwise —
/// an empty vector for a member that is present and empty, which is the distinction the rule
/// is about.
///
/// Read through the XML tree rather than by substring, so the set is the one of THAT gateway
/// and the members read are the appliance's own.
fn reported_appliances(case: &str, body: &str, nat_gateway_id: &str) -> Option<Vec<Appliance>> {
    let parsed = document(case, body);
    let item = parsed
        .descendants()
        .find(|node| {
            node.is_element()
                && child_text(*node, "natGatewayId").as_deref() == Some(nat_gateway_id)
        })
        .unwrap_or_else(|| panic!("case {case:?}: {nat_gateway_id} is not in the report {body}"));
    let set = child(item, "attachedApplianceSet")?;
    Some(
        set.children()
            .filter(roxmltree::Node::is_element)
            .map(Appliance::from_item)
            .collect(),
    )
}

/// The rule: every gateway a read reports carries its `attachedApplianceSet` — empty when it
/// holds no appliance, one entry per appliance otherwise — whichever way the gateway reached
/// the account, in the whole-store report and in one narrowed to its id alike, and a second
/// read answers exactly the same.
#[tokio::test]
async fn every_reported_nat_gateway_carries_its_attached_appliance_set() {
    let (router, engine) = super::ec2_query::emulator();
    let subnet_id = subnet("the network", &router, "10.80.0.0/16", "10.80.1.0/24").await;

    let mut rows: Vec<Row> = Vec::new();

    // Gateways minted through the front door: a public one with the single Elastic IP address
    // a create needs, a public one with the two such a gateway may hold, and a private one,
    // which associates no Elastic IP address at all. None of them has an appliance attached,
    // so each must report an EMPTY set rather than no set.
    let one_address = allocation("a public gateway with one Elastic IP address", &router).await;
    rows.push(Row {
        case: "a created public gateway with one Elastic IP address".to_string(),
        nat_gateway_id: create_gateway(
            "a created public gateway with one Elastic IP address",
            &router,
            &[
                param("SubnetId", &subnet_id),
                param("ConnectivityType", CONNECTIVITY_TYPE_PUBLIC),
                param("AllocationId", &one_address),
            ],
        )
        .await,
        expected: Vec::new(),
    });

    let primary = allocation("a public gateway with two Elastic IP addresses", &router).await;
    let secondary = allocation("a public gateway with two Elastic IP addresses", &router).await;
    rows.push(Row {
        case: "a created public gateway with two Elastic IP addresses".to_string(),
        nat_gateway_id: create_gateway(
            "a created public gateway with two Elastic IP addresses",
            &router,
            &[
                param("SubnetId", &subnet_id),
                param("ConnectivityType", CONNECTIVITY_TYPE_PUBLIC),
                param("AllocationId", &primary),
                param("SecondaryAllocationId.1", &secondary),
            ],
        )
        .await,
        expected: Vec::new(),
    });

    rows.push(Row {
        case: "a created private gateway".to_string(),
        nat_gateway_id: create_gateway(
            "a created private gateway",
            &router,
            &[
                param("SubnetId", &subnet_id),
                param("ConnectivityType", CONNECTIVITY_TYPE_PRIVATE),
            ],
        )
        .await,
        expected: Vec::new(),
    });

    // Stored gateways: one whose record carries no appliance list at all, one whose list is
    // there and empty — both "nothing is attached", so both answer the same empty set — one
    // holding a single appliance, and one holding an appliance in EVERY member of the modelled
    // `NatGatewayApplianceState` catalog (`ec2::ops::types::NAT_GATEWAY_APPLIANCE_STATE_VALUES`),
    // which is also what covers an appliance that reports a failure beside one that does not.
    rows.push(seed(
        &engine,
        "a stored gateway with no appliance list at all",
        &subnet_id,
        None,
    ));
    rows.push(seed(
        &engine,
        "a stored gateway whose appliance list is empty",
        &subnet_id,
        Some(Vec::new()),
    ));
    rows.push(seed(
        &engine,
        "a stored gateway holding one appliance",
        &subnet_id,
        Some(vec![appliance(0, NAT_GATEWAY_APPLIANCE_STATE_VALUES[0])]),
    ));
    rows.push(seed(
        &engine,
        "a stored gateway holding one appliance per modelled attachment state",
        &subnet_id,
        Some(
            NAT_GATEWAY_APPLIANCE_STATE_VALUES
                .iter()
                .enumerate()
                .map(|(index, state)| appliance(index, state))
                .collect(),
        ),
    ));

    // The whole store, then each gateway on its own, then the whole store again: the same
    // answer every time, a read changing nothing a later caller sees.
    let whole_store = ok("the whole store", &router, "DescribeNatGateways", &[]).await;
    for row in &rows {
        let case = &row.case;
        let reported = reported_appliances(case, &whole_store, &row.nat_gateway_id);
        assert_eq!(
            reported.as_deref(),
            Some(row.expected.as_slice()),
            "case {case:?}: expected the gateway to report the appliances {:?} (a present, \
             possibly empty attachedApplianceSet), got {reported:?} in {whole_store}",
            row.expected
        );

        let narrowed = ok(
            case,
            &router,
            "DescribeNatGateways",
            &[param("NatGatewayId.1", &row.nat_gateway_id)],
        )
        .await;
        let reported = reported_appliances(case, &narrowed, &row.nat_gateway_id);
        assert_eq!(
            reported.as_deref(),
            Some(row.expected.as_slice()),
            "case {case:?}: expected a report narrowed to this gateway to carry the same \
             appliances {:?}, got {reported:?} in {narrowed}",
            row.expected
        );
    }

    let again = ok("the whole store, read again", &router, "DescribeNatGateways", &[]).await;
    for row in &rows {
        let case = &row.case;
        let reported = reported_appliances(case, &again, &row.nat_gateway_id);
        assert_eq!(
            reported.as_deref(),
            Some(row.expected.as_slice()),
            "case {case:?}: expected the second read to report the appliances {:?}, got \
             {reported:?} in {again}",
            row.expected
        );
    }
}
