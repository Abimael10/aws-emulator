// SPDX-License-Identifier: CC0-1.0

//! The STATUS a created network interface is reported in, by the create and by every read.
//!
//! Driven over the wire this service speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a network interface's `Status` is decided by WHAT HOLDS IT, and
//! `available` — the resting state of an interface nothing holds
//! (`smithy_query(query="shape",shape="NetworkInterfaceStatus")`, whose value set is
//! `available`, `associated`, `attaching`, `in-use`, `detaching`) — is what a create answers
//! and what every later read reports, until something attaches it. An interface that is
//! already HELD when it reaches the store — one another Amazon Web Services service created
//! to serve a resource of the account — is reported in the status that holds it (`in-use`)
//! instead, which is the boundary of the rule. No read ever reports a value outside the
//! modelled set, and no caller has to wait for the status to settle: nothing is owed on the
//! interface by the clock.
//!
//! The rows differ along what the create carries (nothing beyond the subnet, a description
//! and a tag, a second interface beside the first) and along which of the account's subnets
//! it sits in, because none of that bears on the status; and the interface is read back both
//! by `NetworkInterfaceId` and enumerated by a `vpc-id` filter after further calls, because
//! neither bears on it either.
//!
//! Beside the status, each row pins the two members a created interface carries that follow
//! from the interface rather than from the request: the principal that created it
//! (`NetworkInterface$RequesterId`) and its public hostname
//! (`NetworkInterface$PublicDnsName`), which is the EMPTY name for an interface holding no
//! public address — reported either way, so an absent member cannot be read as a name.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, NETWORK_INTERFACE_STATUS_AVAILABLE,
    NETWORK_INTERFACE_STATUS_IN_USE, NetworkInterfaceSpec,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, filters, indexed};

/// The Region every call here is signed for; the account's default subnets are seeded for it.
const REGION: &str = "us-east-1";

/// The values `NetworkInterfaceStatus` publishes
/// (`smithy_query(query="shape",shape="NetworkInterfaceStatus")`): a reported status is one
/// of these, whichever path reported it.
const NETWORK_INTERFACE_STATUS_VALUES: [&str; 5] = [
    "available",
    "associated",
    "attaching",
    "in-use",
    "detaching",
];

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The account's default subnet ids, in store order (one per zone of the Region).
fn subnet_ids(engine: &engine::InMemoryEngine) -> Vec<String> {
    engine
        .state::<Ec2State>()
        .lock()
        .subnets
        .keys()
        .cloned()
        .collect()
}

/// The VPC of one of the account's subnets.
fn vpc_of_subnet(engine: &engine::InMemoryEngine, subnet_id: &str) -> String {
    engine
        .state::<Ec2State>()
        .lock()
        .subnets
        .get(subnet_id)
        .and_then(|subnet| subnet.vpc_id.clone())
        .unwrap_or_default()
}

/// One call that must be served, as its body.
async fn ok(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The status `DescribeNetworkInterfaces` reports for one interface.
async fn described_status(case: &str, router: &MultiServiceRouter, id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &indexed("NetworkInterfaceId", &[id]),
    )
    .await;
    element(&body, "status")
        .unwrap_or_else(|| panic!("case {case:?}: the reported interface carries no status: {body}"))
}

/// One create whose interface must be reported `available`.
#[derive(Debug, Clone)]
struct CreateRow {
    /// What the row is about.
    case: &'static str,
    /// Which of the account's subnets the interface sits in.
    subnet_index: usize,
    /// The fields the create carries beyond `SubnetId`.
    extra: Vec<(String, String)>,
}

/// Rule: a create answers the interface `available`, and so does every read of it.
#[tokio::test]
async fn a_created_interface_is_available_from_the_create_on() {
    let (router, engine) = super::ec2_query::emulator();
    let subnets = subnet_ids(&engine);
    assert!(
        subnets.len() >= 2,
        "the account's default VPC must have a subnet per zone, got {subnets:?}"
    );

    let rows = [
        CreateRow {
            case: "the subnet alone",
            subnet_index: 0,
            extra: Vec::new(),
        },
        CreateRow {
            case: "a description and a tag",
            subnet_index: 1,
            extra: vec![
                param("Description", "a described interface"),
                param("TagSpecification.1.ResourceType", "network-interface"),
                param("TagSpecification.1.Tag.1.Key", "Name"),
                param("TagSpecification.1.Tag.1.Value", "web"),
            ],
        },
        CreateRow {
            case: "a second interface in the first subnet",
            subnet_index: 0,
            extra: vec![param("Description", "a second interface")],
        },
    ];

    for CreateRow {
        case,
        subnet_index,
        extra,
    } in rows
    {
        let subnet_id = subnets[subnet_index].clone();
        let mut params = vec![param("SubnetId", &subnet_id)];
        params.extend(extra);
        let created = ok(case, &router, "CreateNetworkInterface", &params).await;
        let network_interface_id = element(&created, "networkInterfaceId")
            .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));

        // The create's OWN answer: nothing holds the interface, so it rests in `available`.
        let created_status = element(&created, "status");
        assert_eq!(
            created_status.as_deref(),
            Some(NETWORK_INTERFACE_STATUS_AVAILABLE),
            "case {case:?}: expected the create to answer \
             {NETWORK_INTERFACE_STATUS_AVAILABLE:?}, got {created_status:?} in {created}"
        );
        // Nothing holds it, so the create reports no attachment either.
        assert!(
            element(&created, "attachment").is_none(),
            "case {case:?}: an interface nothing holds carries no attachment, got {created}"
        );
        // And the two members that follow from the interface rather than the request: the
        // principal that created it, and the empty public hostname of an interface holding no
        // public address.
        let requester = element(&created, "requesterId");
        assert!(
            requester.as_deref().is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the create to name the principal that created the \
             interface, got {requester:?} in {created}"
        );
        assert!(
            created.contains("<publicDnsName></publicDnsName>")
                || created.contains("<publicDnsName/>"),
            "case {case:?}: an interface with no public address reports the EMPTY public \
             hostname, got {created}"
        );

        // The read taken straight after the create reports the same status: a caller that
        // waits for `available` before attaching has nothing to wait for.
        let read = described_status(case, &router, &network_interface_id).await;
        assert_eq!(
            read, NETWORK_INTERFACE_STATUS_AVAILABLE,
            "case {case:?}: the first read after the create must report \
             {NETWORK_INTERFACE_STATUS_AVAILABLE:?}, got {read:?}"
        );
        assert!(
            NETWORK_INTERFACE_STATUS_VALUES.contains(&read.as_str()),
            "case {case:?}: a reported status is one of the modelled \
             {NETWORK_INTERFACE_STATUS_VALUES:?}, got {read:?}"
        );

        // And it STAYS there, however many further calls the account makes: the status is
        // the interface's resting state rather than a transient the next caller settles.
        ok(case, &router, "DescribeVpcs", &[]).await;
        ok(case, &router, "DescribeSubnets", &[]).await;
        let again = described_status(case, &router, &network_interface_id).await;
        assert_eq!(
            again, read,
            "case {case:?}: a later read must report the same status, got {again:?} after \
             {read:?}"
        );

        // Enumerated by the VPC rather than by its own id — a different read path, same
        // status: the interface is reported, and every interface the VPC enumerates carries
        // a modelled status.
        let vpc_id = vpc_of_subnet(&engine, &subnet_id);
        let enumerated = ok(
            case,
            &router,
            "DescribeNetworkInterfaces",
            &filters(&[("vpc-id", &[vpc_id.as_str()])]),
        )
        .await;
        assert!(
            enumerated.contains(&network_interface_id),
            "case {case:?}: the VPC must enumerate the interface, got {enumerated}"
        );
        let statuses = elements(&enumerated, "status");
        assert!(
            !statuses.is_empty()
                && statuses
                    .iter()
                    .all(|status| status == NETWORK_INTERFACE_STATUS_AVAILABLE),
            "case {case:?}: every unattached interface the VPC enumerates must be \
             {NETWORK_INTERFACE_STATUS_AVAILABLE:?}, got {statuses:?}"
        );
    }
}

/// The boundary of that rule: an interface that is already HELD when it reaches the store —
/// one another Amazon Web Services service created to serve a resource of the account — is
/// reported in the status that holds it rather than `available`.
#[tokio::test]
async fn an_interface_a_service_holds_is_in_use() {
    let (router, engine) = super::ec2_query::emulator();
    let subnets = subnet_ids(&engine);
    let case = "an interface a service holds";

    let network_interface_id = {
        let store = engine.state::<Ec2State>();
        let mut guard = store.lock();
        let subnet = guard
            .subnets
            .get(&subnets[0])
            .expect("the account's default subnet")
            .clone();
        let cidr_block = subnet.cidr_block.clone().unwrap_or_default();
        let address = guard
            .mint_private_ip_in_cidr(&cidr_block)
            .expect("the default subnet has a free address");
        guard
            .insert_network_interface(NetworkInterfaceSpec {
                subnet_id: subnets[0].clone(),
                vpc_id: subnet.vpc_id.clone().unwrap_or_default(),
                availability_zone: subnet.availability_zone.clone().unwrap_or_default(),
                availability_zone_id: subnet.availability_zone_id.clone().unwrap_or_default(),
                region: REGION.to_string(),
                interface_type: "interface".to_string(),
                private_ip_addresses: vec![address],
                requester: Some("amazon-elb".to_string()),
                ..NetworkInterfaceSpec::default()
            })
            .expect("the store holds the seeded interface")
    };

    let status = described_status(case, &router, &network_interface_id).await;
    assert_eq!(
        status, NETWORK_INTERFACE_STATUS_IN_USE,
        "case {case:?}: expected {NETWORK_INTERFACE_STATUS_IN_USE:?} for an interface a \
         service holds, got {status:?}"
    );

    // And the two statuses do not drift into each other: an interface created beside the
    // held one is `available`, and the held one keeps the status that holds it.
    let created = ok(
        case,
        &router,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnets[1])],
    )
    .await;
    let unheld = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));
    let unheld_status = described_status(case, &router, &unheld).await;
    assert_eq!(
        unheld_status, NETWORK_INTERFACE_STATUS_AVAILABLE,
        "case {case:?}: the interface created beside it is held by nothing, got \
         {unheld_status:?}"
    );
    let later = described_status(case, &router, &network_interface_id).await;
    assert_eq!(
        later, NETWORK_INTERFACE_STATUS_IN_USE,
        "case {case:?}: an interface a service holds keeps the status that holds it, got \
         {later:?}"
    );
}
