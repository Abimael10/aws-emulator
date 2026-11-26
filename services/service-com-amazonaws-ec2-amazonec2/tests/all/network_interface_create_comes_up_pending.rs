// SPDX-License-Identifier: CC0-1.0

//! A created network interface COMES UP: `CreateNetworkInterface` answers it `pending` and the
//! clock makes it `available`.
//!
//! Driven over the wire this service speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=<Operation>`, answered
//! with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a resource EC2 builds asynchronously is answered in the non-terminal
//! status it is being built in, and the clock — not the next caller — moves it to the terminal
//! one. So the create's own answer and a read taken straight after it report `pending`, a read
//! taken after the span the move is owed in reports `available`, and nothing a caller does in
//! between is needed to make the move. An interface that is NOT being brought up — one another
//! service's resource is served by, which is held from the moment it is stored — is reported in
//! the status that holds it instead, which is the boundary of the rule.
//!
//! The rows differ along what the create carries (nothing beyond the subnet, a description and
//! a tag, a second interface beside the first) and along which of the account's subnets it sits
//! in, because none of that bears on the status: the status follows from the interface being
//! brought up.
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

use super::ec2_query::{call, element, indexed};

/// The Region every call here is signed for; the account's default subnets are seeded for it.
const REGION: &str = "us-east-1";

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

/// The interface's status once the clock has had the chance to bring it up: polled until it is
/// `available` or a deadline many times the owed span has passed, so the outcome is the move
/// rather than how fast the host runs.
async fn status_once_up(case: &str, router: &MultiServiceRouter, id: &str) -> String {
    let deadline = Instant::now() + TRANSITION_DELAY * 20;
    loop {
        let status = described_status(case, router, id).await;
        if status == NETWORK_INTERFACE_STATUS_AVAILABLE || Instant::now() >= deadline {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One create whose interface must come up.
#[derive(Debug, Clone)]
struct CreateRow {
    /// What the row is about.
    case: &'static str,
    /// Which of the account's subnets the interface sits in.
    subnet_index: usize,
    /// The fields the create carries beyond `SubnetId`.
    extra: Vec<(String, String)>,
}

/// Rule: a create answers the interface `pending`, and the clock — not a later caller — makes
/// it `available`.
#[tokio::test]
async fn a_created_interface_is_pending_until_the_clock_makes_it_available() {
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
                param("Description", "coming up"),
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
        let mut params = vec![param("SubnetId", &subnets[subnet_index])];
        params.extend(extra);
        let created = ok(case, &router, "CreateNetworkInterface", &params).await;
        let network_interface_id = element(&created, "networkInterfaceId")
            .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));

        // The create's OWN answer: the interface is being brought up.
        let created_status = element(&created, "status");
        assert_eq!(
            created_status.as_deref(),
            Some(NETWORK_INTERFACE_STATUS_PENDING),
            "case {case:?}: expected the create to answer \
             {NETWORK_INTERFACE_STATUS_PENDING:?}, got {created_status:?} in {created}"
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

        // Nothing but the clock is asked, and the move comes: a read is a read, so the
        // interface reaches `available` because its instant passed.
        let settled = status_once_up(case, &router, &network_interface_id).await;
        assert_eq!(
            settled, NETWORK_INTERFACE_STATUS_AVAILABLE,
            "case {case:?}: one owed span after the create the interface must be \
             {NETWORK_INTERFACE_STATUS_AVAILABLE:?}, got {settled:?}"
        );
        // And it STAYS there: the move is applied once, so a second read reports the same.
        let again = described_status(case, &router, &network_interface_id).await;
        assert_eq!(
            again, settled,
            "case {case:?}: a second read must report the same status, got {again:?} after \
             {settled:?}"
        );
    }
}

/// The boundary of that rule: an interface that is already HELD when it reaches the store —
/// one another Amazon Web Services service created to serve a resource of the account — is
/// never `pending`, because nothing is bringing it up.
#[tokio::test]
async fn an_interface_a_service_holds_is_not_coming_up() {
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

    // And it stays held once the clock HAS moved: an interface created beside it comes up,
    // which is what says the owed span has passed, and the held one has not moved with it.
    let created = ok(
        case,
        &router,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnets[1])],
    )
    .await;
    let coming_up = element(&created, "networkInterfaceId")
        .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));
    let settled = status_once_up(case, &router, &coming_up).await;
    assert_eq!(
        settled, NETWORK_INTERFACE_STATUS_AVAILABLE,
        "case {case:?}: the interface created beside it must come up, got {settled:?}"
    );
    let later = described_status(case, &router, &network_interface_id).await;
    assert_eq!(
        later, NETWORK_INTERFACE_STATUS_IN_USE,
        "case {case:?}: an interface nothing is bringing up keeps the status that holds it, \
         got {later:?}"
    );
}
