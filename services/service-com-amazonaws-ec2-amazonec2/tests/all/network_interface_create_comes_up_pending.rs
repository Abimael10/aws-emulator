// SPDX-License-Identifier: CC0-1.0

//! The STATUS a created network interface is reported in, by the create and by every read.
//!
//! Driven over the wire this service speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: an elastic network interface a caller creates is handed back in a
//! NON-TERMINAL status — `pending`, which is what `CreateNetworkInterface` answers — and the
//! CLOCK owes it the resting status of an interface nothing holds, `available`, whether or not
//! anybody calls again; an interface that is already HELD when it reaches the store — one
//! another Amazon Web Services service created to serve a resource of the account — is
//! reported in the status that holds it (`in-use`) from the call that placed it, with nothing
//! owed on it. (`pending` is not a member of the modelled `NetworkInterfaceStatus`
//! (`smithy_query(query="shape",shape="NetworkInterfaceStatus")`, whose value set is
//! `available`, `associated`, `attaching`, `in-use`, `detaching`), so the status a create
//! answers is recorded behaviour; every status a READ reports is a member of that set.)
//!
//! The rows differ along what the create carries (nothing beyond the subnet, a description and
//! a tag, a second interface beside the first) and along which of the account's subnets it
//! sits in, because none of that bears on the status; and the settled interface is read back
//! both by `NetworkInterfaceId` and enumerated by a `vpc-id` filter, because neither bears on
//! it either.
//!
//! Nothing here waits for a span: every assertion about the move the clock owes LOOKS AGAIN
//! until the status it owes is reported or a deadline far past this service's own transition
//! span passes, so no row depends on how fast the host runs.
//!
//! Beside the status, each row pins the two members a created interface carries that follow
//! from the interface rather than from the request: the principal that created it
//! (`NetworkInterface$RequesterId`) and its public hostname
//! (`NetworkInterface$PublicDnsName`), which is the EMPTY name for an interface holding no
//! public address — reported either way, so an absent member cannot be read as a name.

use std::time::{Duration, Instant};

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, NETWORK_INTERFACE_STATUS_AVAILABLE,
    NETWORK_INTERFACE_STATUS_IN_USE, NETWORK_INTERFACE_STATUS_PENDING, NetworkInterfaceSpec,
    TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, filters, indexed};

/// The Region every call here is signed for; the account's default subnets are seeded for it.
const REGION: &str = "us-east-1";

/// The values `NetworkInterfaceStatus` publishes
/// (`smithy_query(query="shape",shape="NetworkInterfaceStatus")`): a status a READ reports is
/// one of these, whichever path reported it.
const NETWORK_INTERFACE_STATUS_VALUES: [&str; 5] = [
    "available",
    "associated",
    "attaching",
    "in-use",
    "detaching",
];

/// How long a poll below keeps looking before it gives up: a large multiple of this service's
/// own transition span, so all the deadline bounds is a hang rather than a slow host.
fn settle_deadline() -> Duration {
    TRANSITION_DELAY * 120
}

/// How long one look waits before the next, short enough that the deadline above costs many
/// looks rather than one.
fn settle_poll() -> Duration {
    TRANSITION_DELAY / 10
}

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

/// Look at the interface AGAIN until the describe reports it in `wanted`, or a deadline far
/// past this service's own transition span passes — the condition rather than a span, so a
/// loaded host only polls more often.
async fn poll_until_status(
    case: &str,
    router: &MultiServiceRouter,
    id: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + settle_deadline();
    loop {
        let status = described_status(case, router, id).await;
        if status == wanted {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock owes the interface {wanted:?}, but it still reports \
             {status:?}"
        );
        tokio::time::sleep(settle_poll()).await;
    }
}

/// One create whose interface must come up `pending` and settle `available`.
#[derive(Debug, Clone)]
struct CreateRow {
    /// What the row is about.
    case: &'static str,
    /// Which of the account's subnets the interface sits in.
    subnet_index: usize,
    /// The fields the create carries beyond `SubnetId`.
    extra: Vec<(String, String)>,
}

/// Rule: a create answers the interface `pending`, and the clock owes it `available`.
#[tokio::test]
async fn a_created_interface_comes_up_pending_and_settles_available() {
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

        // The create's OWN answer: the interface is still coming up.
        let created_status = element(&created, "status");
        assert_eq!(
            created_status.as_deref(),
            Some(NETWORK_INTERFACE_STATUS_PENDING),
            "case {case:?}: expected the create to answer {NETWORK_INTERFACE_STATUS_PENDING:?}, \
             got {created_status:?} in {created}"
        );
        // Nothing holds the interface, so the create reports no attachment either.
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

        // The move the clock owes it, whether or not anybody calls again: `available`, the
        // resting status of an interface nothing holds. Looked at AGAIN until it is reported.
        let settled = poll_until_status(
            case,
            &router,
            &network_interface_id,
            NETWORK_INTERFACE_STATUS_AVAILABLE,
        )
        .await;
        assert!(
            NETWORK_INTERFACE_STATUS_VALUES.contains(&settled.as_str()),
            "case {case:?}: a reported status is one of the modelled \
             {NETWORK_INTERFACE_STATUS_VALUES:?}, got {settled:?}"
        );

        // And it STAYS there, however many further calls the account makes: `available` is
        // the interface's resting state, not a transient the next caller moves on.
        ok(case, &router, "DescribeVpcs", &[]).await;
        ok(case, &router, "DescribeSubnets", &[]).await;
        let again = described_status(case, &router, &network_interface_id).await;
        assert_eq!(
            again, settled,
            "case {case:?}: a later read must report the same status, got {again:?} after \
             {settled:?}"
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
                && statuses.iter().all(|status| {
                    status == NETWORK_INTERFACE_STATUS_AVAILABLE
                        || status == NETWORK_INTERFACE_STATUS_PENDING
                }),
            "case {case:?}: every interface the VPC enumerates is one a create left coming up \
             or one the clock has settled, got {statuses:?}"
        );
    }
}

/// Rule, the other half: a caller does not have to wait the move out before using the
/// interface it was handed — the interface the create answered is the one the account holds,
/// and the delete of an interface nothing holds is served while it is still coming up.
#[tokio::test]
async fn an_interface_still_coming_up_is_the_one_the_account_holds() {
    let (router, engine) = super::ec2_query::emulator();
    let subnets = subnet_ids(&engine);
    let case = "an interface still coming up";

    let created = ok(
        case,
        &router,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnets[0])],
    )
    .await;
    let network_interface_id = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));
    assert_eq!(
        element(&created, "status").as_deref(),
        Some(NETWORK_INTERFACE_STATUS_PENDING),
        "case {case:?}: the create answers {NETWORK_INTERFACE_STATUS_PENDING:?}, got {created}"
    );
    // The record the create wrote, read straight out of the store: the interface exists with
    // the subnet, the VPC and the status the create answered, so the status it comes up in is
    // state rather than a value the answer invented.
    {
        let store = engine.state::<Ec2State>();
        let guard = store.lock();
        let held = guard
            .network_interfaces
            .get(&network_interface_id)
            .unwrap_or_else(|| panic!("case {case:?}: the store must hold the created interface"));
        assert_eq!(
            held.subnet_id.as_deref(),
            Some(subnets[0].as_str()),
            "case {case:?}: the stored interface sits in the subnet the create named, got {:?}",
            held.subnet_id
        );
        assert_eq!(
            held.status.as_deref(),
            Some(NETWORK_INTERFACE_STATUS_PENDING),
            "case {case:?}: the stored interface carries the status the create answered, got \
             {:?}",
            held.status
        );
    }

    // A delete taken while the interface is still coming up is served: nothing HOLDS it, and
    // that — not the status it is passing through — is what a delete refuses on.
    let deleted = ok(
        case,
        &router,
        "DeleteNetworkInterface",
        &[param("NetworkInterfaceId", &network_interface_id)],
    )
    .await;
    assert!(
        deleted.contains("DeleteNetworkInterfaceResponse"),
        "case {case:?}: the delete must answer its own envelope, got {deleted}"
    );
    let gone = ok(case, &router, "DescribeNetworkInterfaces", &[]).await;
    assert!(
        !gone.contains(&network_interface_id),
        "case {case:?}: a deleted interface is reported by nothing, got {gone}"
    );
}

/// The boundary of the rule: an interface that is already HELD when it reaches the store —
/// one another Amazon Web Services service created to serve a resource of the account — is
/// reported in the status that holds it rather than coming up at all.
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

    // And the two lifecycles do not drift into each other: an interface created beside the
    // held one comes up and settles `available`, and the held one keeps the status that holds
    // it throughout.
    let created = ok(
        case,
        &router,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnets[1])],
    )
    .await;
    let unheld = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));
    let unheld_status = poll_until_status(
        case,
        &router,
        &unheld,
        NETWORK_INTERFACE_STATUS_AVAILABLE,
    )
    .await;
    assert_eq!(
        unheld_status, NETWORK_INTERFACE_STATUS_AVAILABLE,
        "case {case:?}: the interface created beside it is held by nothing, so the clock \
         settles it {NETWORK_INTERFACE_STATUS_AVAILABLE:?}, got {unheld_status:?}"
    );
    let later = described_status(case, &router, &network_interface_id).await;
    assert_eq!(
        later, NETWORK_INTERFACE_STATUS_IN_USE,
        "case {case:?}: an interface a service holds keeps the status that holds it, got \
         {later:?}"
    );
}
