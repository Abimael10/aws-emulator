// SPDX-License-Identifier: CC0-1.0

//! Who a network interface was created BY, and the public hostname it holds, on the read path —
//! driven over the wire this service speaks (`ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML; README.md / OPERATIONS.md
//! "Wire protocol").
//!
//! The rule under test: every reported network interface carries the alias or account of the
//! principal or service that created it (`NetworkInterface$RequesterId`) and its public
//! hostname (`NetworkInterface$PublicDnsName`) — the EMPTY name for an interface that holds no
//! public address, reported either way, so an absent member can never be read as a name a
//! client should use. The attribution follows from the interface rather than from the request,
//! which is why the two kinds of interface answer it differently: one the caller created for
//! itself names the caller's own principal and is managed by nothing, while one another Amazon
//! Web Services service created to serve a resource of the account names that service and is
//! managed by it.
//!
//! The rows span that class — a created interface, a created interface with a description and
//! tags, and a service's own — and each is read through the describe rather than off the
//! create, because the read path is where a client that did not make the create looks.

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use ec2::state::NetworkInterfaceSpec;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, indexed};
use super::wire;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The `RequesterId` an interface another service created for a resource of the account
/// carries: the alias of that service.
const MANAGING_SERVICE: &str = "amazon-elb";

/// One form field.
fn param(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
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

/// What the read path reports about one interface's attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Attribution {
    /// `NetworkInterface$RequesterId`.
    requester_id: Option<String>,
    /// `NetworkInterface$RequesterManaged`.
    requester_managed: Option<String>,
    /// `NetworkInterface$PublicDnsName`.
    public_dns_name: Option<String>,
}

/// The attribution `DescribeNetworkInterfaces` reports for one interface.
async fn described(case: &str, router: &MultiServiceRouter, id: &str) -> Attribution {
    let body = ok(
        case,
        router,
        "DescribeNetworkInterfaces",
        &indexed("NetworkInterfaceId", &[id]),
    )
    .await;
    let document = wire::document(case, &body);
    let root = document.root_element();
    let item = wire::set_items(root, "networkInterfaceSet")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the interface must be reported: {body}"));
    Attribution {
        requester_id: wire::child_text(item, "requesterId"),
        requester_managed: wire::child_text(item, "requesterManaged"),
        public_dns_name: wire::child_text(item, "publicDnsName"),
    }
}

/// Rule: a reported interface names who created it and carries its public hostname.
#[tokio::test]
async fn a_reported_interface_names_who_created_it_and_its_public_hostname() {
    let (router, engine) = super::ec2_query::emulator();
    let subnets: Vec<String> = engine
        .state::<Ec2State>()
        .lock()
        .subnets
        .keys()
        .cloned()
        .collect();

    // The caller's own interfaces, differing in what the create carried.
    let rows: [(&str, Vec<(String, String)>); 2] = [
        ("the subnet alone", Vec::new()),
        (
            "a description and a tag",
            vec![
                param("Description", "attributed"),
                param("TagSpecification.1.ResourceType", "network-interface"),
                param("TagSpecification.1.Tag.1.Key", "Name"),
                param("TagSpecification.1.Tag.1.Value", "web"),
            ],
        ),
    ];
    for (case, extra) in rows {
        let mut params = vec![param("SubnetId", &subnets[0])];
        params.extend(extra);
        let created = ok(case, &router, "CreateNetworkInterface", &params).await;
        let id = super::ec2_query::element(&created, "networkInterfaceId")
            .unwrap_or_else(|| panic!("case {case:?}: the create must answer an id: {created}"));

        let reported = described(case, &router, &id).await;
        assert!(
            reported
                .requester_id
                .as_deref()
                .is_some_and(|requester| !requester.is_empty()),
            "case {case:?}: the read path names the principal that created the interface, got \
             {reported:?}"
        );
        assert_eq!(
            reported.requester_managed.as_deref(),
            Some("false"),
            "case {case:?}: nothing manages an interface the caller created, got {reported:?}"
        );
        assert_eq!(
            reported.public_dns_name.as_deref(),
            Some(""),
            "case {case:?}: an interface holding no public address reports the EMPTY public \
             hostname, got {reported:?}"
        );
    }

    // And a service's own interface: the attribution is that service, which is also what
    // makes the interface one Amazon Web Services manages.
    let case = "an interface a service created for a resource of the account";
    let managed = {
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
                requester: Some(MANAGING_SERVICE.to_string()),
                ..NetworkInterfaceSpec::default()
            })
            .expect("the store holds the seeded interface")
    };
    let reported = described(case, &router, &managed).await;
    assert_eq!(
        reported,
        Attribution {
            requester_id: Some(MANAGING_SERVICE.to_string()),
            requester_managed: Some("true".to_string()),
            public_dns_name: Some(String::new()),
        },
        "case {case:?}: expected the service's own attribution, got {reported:?}"
    );
}
