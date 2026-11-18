// SPDX-License-Identifier: CC0-1.0

//! WHICH interfaces an EC2 describe selects when a load balancer's own ones sit in the
//! account's inventory beside interfaces it does not own.
//!
//! RULE: a load balancer's own elastic network interfaces — one per subnet it is attached to,
//! created and managed by the load balancing service — are ordinary EC2 network interfaces of
//! the account, so every documented filter of `DescribeNetworkInterfaces` is evaluated over
//! them exactly as over any other interface: `vpc-id` selects the ones in the VPC asked about
//! and nothing from another VPC, `requester-id` selects the ones the named service created and
//! leaves out every interface of a different requester, `interface-type` and
//! `requester-managed` select them by the family's modelled type and by their being
//! service-managed, `subnet-id` narrows them to one subnet, filters AND together and the
//! values of one filter OR together; and each selected interface is reported with the private
//! address it holds, drawn from the VPC under diagnosis and held by nothing else. Releasing
//! them is the same rule in reverse: no filter selects an interface of a load balancer that is
//! gone, and every interface the load balancer never owned stays.
//!
//! Two crates have to agree for that — ELBv2 places and releases the interfaces, EC2 holds the
//! inventory and evaluates the filters — so the suite lives here: a crate-local test can only
//! ever see one side of it.
//!
//! `elbv2_host_counts_and_interfaces.rs` asserts WHAT one of those interfaces carries. This
//! suite asserts which ones each FILTER picks out of an inventory that also holds interfaces
//! of the caller and of a second load balancer in another VPC — the question a client doing an
//! address-level diagnosis of its own VPC actually asks, and one a filter evaluation that
//! ignored the name it was given would answer wrongly while still carrying the load balancer's
//! interfaces. The rows differ along what the rule generalises over: which filter names are
//! combined, how many values one of them is given, the VPC asked about, the load balancer
//! family in it, and how many interfaces the answer holds. The configuration an issue happens
//! to mention is a single row.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, opt_element};

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// The API version an ec2Query request carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an awsQuery request naming the ELBv2 model carries.
const ELB_VERSION: &str = "2015-12-01";

/// The alias `NetworkInterface$RequesterId` carries for an interface Elastic Load Balancing
/// created, which is what the `requester-id` filter selects it by.
const SERVICE_REQUESTER: &str = "amazon-elb";

/// An alias of a service that created nothing in this account's inventory, so the same filter
/// given it selects nothing.
const OTHER_REQUESTER: &str = "amazon-rds";

/// The `NetworkInterfaceType` each family's own interfaces carry, from the modelled enum
/// (`smithy_query(query="shape",shape="NetworkInterfaceType")`).
const APPLICATION_INTERFACE_TYPE: &str = "load_balancer";
const NETWORK_INTERFACE_TYPE: &str = "network_load_balancer";

/// The VPC under diagnosis: the load balancer the rows ask about is placed in both its
/// subnets.
const FIRST_VPC_CIDR: &str = "10.71.0.0/16";
const FIRST_SUBNET_CIDRS: [(&str, &str); 2] = [
    ("10.71.1.0/24", "us-east-1a"),
    ("10.71.2.0/24", "us-east-1b"),
];

/// A second VPC holding a load balancer of the other family, so a filter that ignores the VPC
/// it was given, or the family it was given, answers visibly too much.
const SECOND_VPC_CIDR: &str = "10.72.0.0/16";
const SECOND_SUBNET_CIDRS: [(&str, &str); 1] = [("10.72.1.0/24", "us-east-1a")];

/// Two addresses of the first VPC that nothing in the emulator answers on, registered with a
/// target group: the stale targets a diagnosis is trying to tell apart from the load
/// balancer's own addresses. High in the range, so they are not addresses the inventory hands
/// an interface of its own.
const DEAD_ADDRESSES: [&str; 2] = ["10.71.1.200", "10.71.1.201"];

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses, and answer its
/// status and body.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// Drive one ec2Query action that must succeed, and answer its body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// Drive one awsQuery action against the ELBv2 model that must succeed, and answer its body.
async fn elbv2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send(
        router,
        "elasticloadbalancingv2",
        form(action, ELB_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: ELBv2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One VPC of the estate: its id, `(subnet id, CIDR block)` per subnet in the order created,
/// and a security group of it.
struct Vpc {
    id: String,
    subnets: Vec<(String, String)>,
    security_group: String,
}

/// Create one such VPC through EC2 itself, so every value asserted against is one EC2
/// reported rather than one this suite made up.
async fn seed_vpc(
    router: &MultiServiceRouter,
    cidr: &str,
    subnet_cidrs: &[(&str, &str)],
    group_name: &str,
) -> Vpc {
    let id = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", cidr)]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (subnet_cidr, zone) in subnet_cidrs {
        let created = ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", id.as_str()),
                ("CidrBlock", subnet_cidr),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push((element(&created, "subnetId"), (*subnet_cidr).to_string()));
    }
    let security_group = element(
        &ec2(
            router,
            "CreateSecurityGroup",
            &[
                ("GroupName", group_name),
                ("GroupDescription", "the load balancers own VPC"),
                ("VpcId", id.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    Vpc {
        id,
        subnets,
        security_group,
    }
}

/// The inventory the rows read: two VPCs, an interface of the caller beside the load
/// balancer's own, a target group holding the dead addresses, and one load balancer of each
/// family.
struct Estate {
    first: Vpc,
    second: Vpc,
    /// The interface the CALLER created in the load balancer's own subnet, and its address.
    caller_interface: String,
    caller_address: String,
    /// The application load balancer of the first VPC, the interfaces it holds, the addresses
    /// those report, and the subset of them sitting in the first of its two subnets.
    application: String,
    application_interfaces: BTreeSet<String>,
    application_addresses: BTreeSet<String>,
    application_in_first_subnet: BTreeSet<String>,
    /// The network load balancer of the second VPC and the interfaces it holds.
    network_interfaces: BTreeSet<String>,
}

/// Build that inventory, reading each load balancer's own interfaces off what the inventory
/// GAINED when it was created — never off the filter under test.
async fn seed_estate(router: &MultiServiceRouter) -> Estate {
    let first = seed_vpc(
        router,
        FIRST_VPC_CIDR,
        &FIRST_SUBNET_CIDRS,
        "elbv2-filters-first",
    )
    .await;
    let second = seed_vpc(
        router,
        SECOND_VPC_CIDR,
        &SECOND_SUBNET_CIDRS,
        "elbv2-filters-second",
    )
    .await;

    // An interface of the CALLER in the subnet the load balancer is about to be placed in: it
    // is in the VPC, so a filter on the VPC selects it, and no filter naming the load
    // balancing service may.
    let created = ec2(
        router,
        "CreateNetworkInterface",
        &[
            ("SubnetId", first.subnets[0].0.as_str()),
            ("SecurityGroupId.1", first.security_group.as_str()),
            ("Description", "a workload of the account"),
        ],
    )
    .await;
    let caller_interface = element(&created, "networkInterfaceId");
    let caller_address = element(&created, "privateIpAddress");

    // A target group registering two addresses nothing answers on, so "no address the load
    // balancer holds is a registered target's" is asserted against registrations that exist.
    let group = element(
        &elbv2(
            router,
            "CreateTargetGroup",
            &[
                ("Name", "elbv2-filters-targets"),
                ("Protocol", "HTTP"),
                ("Port", "443"),
                ("TargetType", "ip"),
                ("VpcId", first.id.as_str()),
            ],
        )
        .await,
        "TargetGroupArn",
    );
    elbv2(
        router,
        "RegisterTargets",
        &[
            ("TargetGroupArn", group.as_str()),
            ("Targets.member.1.Id", DEAD_ADDRESSES[0]),
            ("Targets.member.2.Id", DEAD_ADDRESSES[1]),
        ],
    )
    .await;

    let before = ids_matching(router, &[("vpc-id", vec![first.id.clone()])]).await;
    let application = element(
        &elbv2(
            router,
            "CreateLoadBalancer",
            &[
                ("Name", "elbv2-filters-alb"),
                ("Scheme", "internal"),
                ("SecurityGroups.member.1", first.security_group.as_str()),
                ("Subnets.member.1", first.subnets[0].0.as_str()),
                ("Subnets.member.2", first.subnets[1].0.as_str()),
            ],
        )
        .await,
        "LoadBalancerArn",
    );
    let application_interfaces: BTreeSet<String> =
        ids_matching(router, &[("vpc-id", vec![first.id.clone()])])
            .await
            .difference(&before)
            .cloned()
            .collect();

    let before = ids_matching(router, &[("vpc-id", vec![second.id.clone()])]).await;
    elbv2(
        router,
        "CreateLoadBalancer",
        &[
            ("Name", "elbv2-filters-nlb"),
            ("Scheme", "internal"),
            ("Type", "network"),
            ("Subnets.member.1", second.subnets[0].0.as_str()),
        ],
    )
    .await;
    let network_interfaces: BTreeSet<String> =
        ids_matching(router, &[("vpc-id", vec![second.id.clone()])])
            .await
            .difference(&before)
            .cloned()
            .collect();

    let mut application_addresses = BTreeSet::new();
    let mut application_in_first_subnet = BTreeSet::new();
    for id in &application_interfaces {
        let (subnet, address) = subnet_and_address(router, id).await;
        application_addresses.insert(address);
        if subnet == first.subnets[0].0 {
            application_in_first_subnet.insert(id.clone());
        }
    }

    Estate {
        first,
        second,
        caller_interface,
        caller_address,
        application,
        application_interfaces,
        application_addresses,
        application_in_first_subnet,
        network_interfaces,
    }
}

/// One row: the filters a diagnosis gives `DescribeNetworkInterfaces`, and the interfaces they
/// select.
struct Row {
    case: &'static str,
    filters: Vec<(&'static str, Vec<String>)>,
    expected: BTreeSet<String>,
}

#[tokio::test]
async fn an_ec2_filter_selects_a_load_balancers_own_interfaces_and_nothing_else() {
    let router = build_router();
    let estate = seed_estate(&router).await;

    // The preconditions the rows read against: one interface per subnet each load balancer is
    // attached to, which is what makes the counts below meaningful.
    assert_eq!(
        (
            estate.application_interfaces.len(),
            estate.network_interfaces.len()
        ),
        (2, 1),
        "setup: expected one interface per attached subnet, got {:?} and {:?}",
        estate.application_interfaces,
        estate.network_interfaces
    );

    let elb = vec![SERVICE_REQUESTER.to_string()];
    let both_vpcs = vec![estate.first.id.clone(), estate.second.id.clone()];
    let service_owned: BTreeSet<String> = estate
        .application_interfaces
        .union(&estate.network_interfaces)
        .cloned()
        .collect();
    let mut everything_in_first: BTreeSet<String> = estate.application_interfaces.clone();
    everything_in_first.insert(estate.caller_interface.clone());

    let rows: Vec<Row> = vec![
        Row {
            case: "the VPC under diagnosis, which holds the caller's interface too",
            filters: vec![("vpc-id", vec![estate.first.id.clone()])],
            expected: everything_in_first.clone(),
        },
        Row {
            case: "that VPC and the service that created the interfaces",
            filters: vec![
                ("vpc-id", vec![estate.first.id.clone()]),
                ("requester-id", elb.clone()),
            ],
            expected: estate.application_interfaces.clone(),
        },
        Row {
            case: "the service alone, across every VPC of the account",
            filters: vec![("requester-id", elb.clone())],
            expected: service_owned.clone(),
        },
        Row {
            case: "both VPCs named as two values of one filter, and that service",
            filters: vec![("vpc-id", both_vpcs), ("requester-id", elb.clone())],
            expected: service_owned,
        },
        Row {
            case: "the other VPC and that service",
            filters: vec![
                ("vpc-id", vec![estate.second.id.clone()]),
                ("requester-id", elb.clone()),
            ],
            expected: estate.network_interfaces.clone(),
        },
        Row {
            case: "a service that created nothing in this account",
            filters: vec![
                ("vpc-id", vec![estate.first.id.clone()]),
                ("requester-id", vec![OTHER_REQUESTER.to_string()]),
            ],
            expected: BTreeSet::new(),
        },
        Row {
            case: "the interface type of the application family, in its own VPC",
            filters: vec![
                ("vpc-id", vec![estate.first.id.clone()]),
                (
                    "interface-type",
                    vec![APPLICATION_INTERFACE_TYPE.to_string()],
                ),
            ],
            expected: estate.application_interfaces.clone(),
        },
        Row {
            case: "the interface type of the network family, in the VPC holding none of it",
            filters: vec![
                ("vpc-id", vec![estate.first.id.clone()]),
                ("interface-type", vec![NETWORK_INTERFACE_TYPE.to_string()]),
            ],
            expected: BTreeSet::new(),
        },
        Row {
            case: "the interfaces a service manages, in the VPC under diagnosis",
            filters: vec![
                ("vpc-id", vec![estate.first.id.clone()]),
                ("requester-managed", vec!["true".to_string()]),
            ],
            expected: estate.application_interfaces.clone(),
        },
        Row {
            case: "the interfaces nothing manages for the account, in that VPC",
            filters: vec![
                ("vpc-id", vec![estate.first.id.clone()]),
                ("requester-managed", vec!["false".to_string()]),
            ],
            expected: [estate.caller_interface.clone()].into_iter().collect(),
        },
        Row {
            case: "one of the two subnets the load balancer is attached to, and its service",
            filters: vec![
                ("subnet-id", vec![estate.first.subnets[0].0.clone()]),
                ("requester-id", elb),
            ],
            expected: estate.application_in_first_subnet.clone(),
        },
    ];

    for row in &rows {
        let selected = ids_matching(&router, &row.filters).await;
        assert_eq!(
            selected, row.expected,
            "case {:?}: expected {:?}, got {selected:?}",
            row.case, row.expected
        );
    }

    // And the addresses: a diagnosis reads them straight off the filtered answer, so the
    // answer carries the private address of each interface it selected — addresses out of the
    // VPC being diagnosed, held by neither the caller's interface nor a registered target.
    let body = describe_filtered(
        &router,
        &[
            ("vpc-id", vec![estate.first.id.clone()]),
            ("requester-id", vec![SERVICE_REQUESTER.to_string()]),
        ],
    )
    .await;
    let listed: BTreeSet<String> = elements(&body, "privateIpAddress").into_iter().collect();
    assert_eq!(
        listed, estate.application_addresses,
        "case \"the addresses of the filtered answer\": expected {:?}, got {listed:?}",
        estate.application_addresses
    );
    // The first two octets of the VPC's own /16, which every address inside it shares.
    let prefix = format!(
        "{}.",
        FIRST_VPC_CIDR
            .split('.')
            .take(2)
            .collect::<Vec<&str>>()
            .join(".")
    );
    for address in &estate.application_addresses {
        assert!(
            address.starts_with(&prefix),
            "case \"an address comes out of the VPC under diagnosis ({FIRST_VPC_CIDR})\": got \
             {address:?}"
        );
        assert!(
            !DEAD_ADDRESSES.contains(&address.as_str()) && *address != estate.caller_address,
            "case \"no address the load balancer holds is held by anything else\": got \
             {address:?} beside {:?} and {DEAD_ADDRESSES:?}",
            estate.caller_address
        );
    }
}

#[tokio::test]
async fn deleting_a_load_balancer_gives_back_its_own_interfaces_alone() {
    let router = build_router();
    let estate = seed_estate(&router).await;

    elbv2(
        &router,
        "DeleteLoadBalancer",
        &[("LoadBalancerArn", estate.application.as_str())],
    )
    .await;

    // Nothing selects an interface of a load balancer that is gone.
    let selected = ids_matching(
        &router,
        &[
            ("vpc-id", vec![estate.first.id.clone()]),
            ("requester-id", vec![SERVICE_REQUESTER.to_string()]),
        ],
    )
    .await;
    assert!(
        selected.is_empty(),
        "case \"the deleted load balancer's own interfaces\": expected none, got {selected:?}"
    );

    // While every interface it never owned stays: the caller's, in the same subnet, and the
    // other load balancer's, in the other VPC.
    let remaining = ids_matching(&router, &[("vpc-id", vec![estate.first.id.clone()])]).await;
    let expected: BTreeSet<String> = [estate.caller_interface.clone()].into_iter().collect();
    assert_eq!(
        remaining, expected,
        "case \"the interfaces of the VPC after the delete\": expected {expected:?}, got \
         {remaining:?}"
    );
    let other = ids_matching(
        &router,
        &[
            ("vpc-id", vec![estate.second.id.clone()]),
            ("requester-id", vec![SERVICE_REQUESTER.to_string()]),
        ],
    )
    .await;
    assert_eq!(
        other, estate.network_interfaces,
        "case \"the other load balancer's interfaces after the delete\": expected {:?}, got \
         {other:?}",
        estate.network_interfaces
    );
}

/// `DescribeNetworkInterfaces` with `filters`, each given as the name and its values, as the
/// wire numbers them (`Filter.<n>.Name` / `Filter.<n>.Value.<m>`).
async fn describe_filtered(
    router: &MultiServiceRouter,
    filters: &[(&str, Vec<String>)],
) -> String {
    let mut params: Vec<(String, String)> = Vec::new();
    for (index, (name, values)) in filters.iter().enumerate() {
        params.push((format!("Filter.{}.Name", index + 1), (*name).to_string()));
        for (at, value) in values.iter().enumerate() {
            params.push((
                format!("Filter.{}.Value.{}", index + 1, at + 1),
                value.clone(),
            ));
        }
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    ec2(router, "DescribeNetworkInterfaces", &borrowed).await
}

/// The id of every interface that read selects.
async fn ids_matching(
    router: &MultiServiceRouter,
    filters: &[(&str, Vec<String>)],
) -> BTreeSet<String> {
    let body = describe_filtered(router, filters).await;
    elements(&body, "networkInterfaceId").into_iter().collect()
}

/// The `(subnet, private address)` EC2 reports for one interface asked for by id — how a
/// client reads an address off an id a filter answered with.
async fn subnet_and_address(router: &MultiServiceRouter, id: &str) -> (String, String) {
    let body = ec2(
        router,
        "DescribeNetworkInterfaces",
        &[("NetworkInterfaceId.1", id)],
    )
    .await;
    (
        opt_element(&body, "subnetId").unwrap_or_default(),
        opt_element(&body, "privateIpAddress").unwrap_or_default(),
    )
}
