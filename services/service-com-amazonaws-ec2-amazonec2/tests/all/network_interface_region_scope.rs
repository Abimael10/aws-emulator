// SPDX-License-Identifier: CC0-1.0

//! `DescribeNetworkInterfaces` is REGION-SCOPED, driven over the wire the service actually
//! speaks — `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rule under test: EC2 is a regional service reached through a regional endpoint, so the
//! network interfaces a describe reports are the ones of the Region the request was signed
//! for. An interface created in another Region is invisible there — a filter value naming its
//! VPC simply matches nothing — and a `NetworkInterfaceIds` entry naming one names no
//! interface at all, which is `InvalidNetworkInterfaceID.NotFound`.
//!
//! The rows are the Regions themselves, and per Region: the account-wide report, a `vpc-id`
//! filter naming EVERY Region's VPC, a `subnet-id` filter naming every Region's subnet, and
//! an id named directly. An interface a LAUNCH created is one row too, because a launch
//! places an interface without a caller naming a subnet. Nothing here asserts a count of
//! interfaces the service seeds itself: every expectation is the set of interfaces this suite
//! created in that Region.

use std::collections::BTreeSet;

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, filters, indexed};

/// The Regions this suite builds one VPC, one subnet and one interface in — three Regions of
/// the same partition, each reached through its own endpoint.
const REGIONS: [&str; 3] = ["us-east-1", "us-west-1", "us-west-2"];

/// An AMI id of the right FORM that this account holds no image for: which image an instance
/// came from has nothing to do with which Region its interfaces are in.
const AMI: &str = "ami-0123456789abcdef0";

/// The EC2 code for an interface id the account holds no interface for IN THE SIGNED REGION.
const NOT_FOUND: &str = "InvalidNetworkInterfaceID.NotFound";

fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// One call that must be served, as its body.
async fn ok(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    params: &[(String, String)],
) -> String {
    let (status, body) = call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action} in {region}, got {status} with \
         body {body}"
    );
    body
}

/// What this suite built in one Region.
struct Built {
    region: &'static str,
    vpc_id: String,
    subnet_id: String,
    /// The interface `CreateNetworkInterface` minted, and the primary interface of an
    /// instance the launch placed.
    interface_ids: BTreeSet<String>,
}

/// Build one VPC, one subnet, one interface and one launched instance in `region`.
async fn build(router: &MultiServiceRouter, region: &'static str, block: u8) -> Built {
    let case = region;
    let created_vpc = ok(
        router,
        case,
        region,
        "CreateVpc",
        &[p("CidrBlock", &format!("10.{block}.0.0/16"))],
    )
    .await;
    let vpc_id = element(&created_vpc, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: CreateVpc answered no vpcId: {created_vpc}"));
    let created_subnet = ok(
        router,
        case,
        region,
        "CreateSubnet",
        &[
            p("VpcId", &vpc_id),
            p("CidrBlock", &format!("10.{block}.1.0/24")),
        ],
    )
    .await;
    let subnet_id = element(&created_subnet, "subnetId").unwrap_or_else(|| {
        panic!("case {case:?}: CreateSubnet answered no subnetId: {created_subnet}")
    });
    let created_interface = ok(
        router,
        case,
        region,
        "CreateNetworkInterface",
        &[p("SubnetId", &subnet_id), p("Description", region)],
    )
    .await;
    let interface_id = element(&created_interface, "networkInterfaceId").unwrap_or_else(|| {
        panic!("case {case:?}: the create answered no networkInterfaceId: {created_interface}")
    });
    let launched = ok(
        router,
        case,
        region,
        "RunInstances",
        &[
            p("ImageId", AMI),
            p("MinCount", "1"),
            p("MaxCount", "1"),
            p("SubnetId", &subnet_id),
        ],
    )
    .await;
    let instance_id = element(&launched, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances answered no instanceId: {launched}"));
    let launched_interface =
        super::ec2_query::primary_network_interface(router, region, case, &instance_id).await;
    Built {
        region,
        vpc_id,
        subnet_id,
        interface_ids: BTreeSet::from([interface_id, launched_interface]),
    }
}

/// The interface ids a describe signed for `region` reports for `params`, as a set.
async fn reported(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    params: &[(String, String)],
) -> BTreeSet<String> {
    let body = ok(router, case, region, "DescribeNetworkInterfaces", params).await;
    elements(&body, "networkInterfaceId").into_iter().collect()
}

/// A Region's describe reports that Region's interfaces and no others, however the request
/// narrows the set.
#[tokio::test]
async fn a_region_describes_only_its_own_network_interfaces() {
    let (router, _engine) = super::ec2_query::emulator();
    let mut built = Vec::new();
    for (block, region) in REGIONS.iter().enumerate() {
        built.push(build(&router, region, u8::try_from(block).unwrap_or_default() + 20).await);
    }

    // Every VPC and subnet of every Region, as a filter naming them all: the one whose
    // Region the call is signed for is the only one that can match.
    let every_vpc: Vec<&str> = built.iter().map(|row| row.vpc_id.as_str()).collect();
    let every_subnet: Vec<&str> = built.iter().map(|row| row.subnet_id.as_str()).collect();
    let every_interface: Vec<String> = built
        .iter()
        .flat_map(|row| row.interface_ids.iter().cloned())
        .collect();

    for row in &built {
        let region = row.region;
        let own = row.interface_ids.clone();

        // The account-wide report of one Region: its own interfaces.
        let case = format!("{region}: no narrowing at all");
        let all = reported(&router, &case, region, &[]).await;
        assert_eq!(
            all, own,
            "case {case:?}: expected exactly this Region's interfaces {own:?}, got {all:?}"
        );

        // A `vpc-id` filter naming EVERY Region's VPC: the other Regions' ids match nothing.
        let case = format!("{region}: a vpc-id filter naming all three VPCs");
        let by_vpc = reported(
            &router,
            &case,
            region,
            &filters(&[("vpc-id", &every_vpc)]),
        )
        .await;
        assert_eq!(
            by_vpc, own,
            "case {case:?}: expected only this Region's own interfaces {own:?}, got {by_vpc:?}"
        );

        // The same for a `subnet-id` filter, so the scope is not a property of one filter.
        let case = format!("{region}: a subnet-id filter naming all three subnets");
        let by_subnet = reported(
            &router,
            &case,
            region,
            &filters(&[("subnet-id", &every_subnet)]),
        )
        .await;
        assert_eq!(
            by_subnet, own,
            "case {case:?}: expected only this Region's own interfaces {own:?}, got \
             {by_subnet:?}"
        );

        // An id named directly: this Region's own resolves, and every other Region's is no
        // interface this request can name.
        for interface_id in &every_interface {
            let mine = own.contains(interface_id);
            let case = format!("{region}: the id {interface_id} named directly");
            let (status, body) = call(
                &router,
                region,
                "DescribeNetworkInterfaces",
                &indexed("NetworkInterfaceId", &[interface_id]),
            )
            .await;
            if mine {
                assert_eq!(
                    status, 200,
                    "case {case:?}: an interface of this Region must resolve, got {status} \
                     with body {body}"
                );
                assert_eq!(
                    elements(&body, "networkInterfaceId"),
                    vec![interface_id.clone()],
                    "case {case:?}: expected exactly that interface, got {body}"
                );
            } else {
                assert_eq!(
                    status, 400,
                    "case {case:?}: an interface of another Region names nothing here, so the \
                     call is refused, got {status} with body {body}"
                );
                assert_eq!(
                    error_code(&body),
                    NOT_FOUND,
                    "case {case:?}: expected {NOT_FOUND}, got {body}"
                );
            }
        }
    }
}
