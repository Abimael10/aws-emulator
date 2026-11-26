// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `GetIpamResourceCidrs` and the `ModifyIpamResourceCidr` that decides what
//! it reports, driven over the wire the service actually speaks: `ec2Query` — an
//! `application/x-www-form-urlencoded` POST whose body carries `Action=…`, answered with XML
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that THE RESOURCE CIDRS OF AN IPAM SCOPE ARE THE RANGES OF THE ACCOUNT'S
//! OWN RESOURCES, DERIVED FROM THE STORE: every IPv4 range of the VPCs and subnets in a Region the
//! scope's IPAM operates in, each reported with the pool of that scope which hands out its space
//! and with the management, compliance and overlap that follow from the store — and the only thing
//! a caller decides about one is whether IPAM monitors it and which private scope of the IPAM it
//! sits in, which `ModifyIpamResourceCidr` records, releasing the pool allocation of a range it
//! stops monitoring.
//!
//! The rows of each table differ along what that generalises over: which kind of resource holds the
//! range, whether the range came out of a pool, whether it still meets that pool's rules, which
//! Region the resource is in, how two ranges of one scope relate (equal, nested, disjoint), which
//! selector or filter the request narrows by, and which member of the request is at fault.
//!
//! Every derived number is asserted as the arithmetic of the store rather than as fixed text: a
//! VPC's IP usage is the share of its range its subnets cover, and a subnet's is the share of its
//! range that is not available to an interface.

use std::collections::BTreeSet;

use ec2::state::{
    INVALID_IPAM_POOL_ID_MALFORMED, INVALID_IPAM_POOL_ID_NOT_FOUND,
    INVALID_IPAM_SCOPE_ID_MALFORMED, INVALID_IPAM_SCOPE_ID_NOT_FOUND, INVALID_PAGINATION_TOKEN,
    INVALID_PARAMETER_VALUE, INVALID_RESOURCE_CIDR_NOT_FOUND, IPAM_COMPLIANCE_STATUS_COMPLIANT,
    IPAM_COMPLIANCE_STATUS_IGNORED, IPAM_COMPLIANCE_STATUS_NONCOMPLIANT,
    IPAM_COMPLIANCE_STATUS_UNMANAGED, IPAM_MANAGEMENT_STATE_IGNORED,
    IPAM_MANAGEMENT_STATE_MANAGED, IPAM_MANAGEMENT_STATE_UNMANAGED,
    IPAM_OVERLAP_STATUS_IGNORED, IPAM_OVERLAP_STATUS_NONOVERLAPPING,
    IPAM_OVERLAP_STATUS_OVERLAPPING, IPAM_RESOURCE_TYPE_SUBNET, IPAM_RESOURCE_TYPE_VPC,
    MISSING_PARAMETER,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for unless it says otherwise, which is therefore each
/// IPAM's home Region and the one operating Region its creation gives it.
const REGION: &str = "us-east-1";

/// Another Region of the same partition, which the IPAMs here do NOT operate in: a resource created
/// there is monitored by nothing.
const UNMONITORED_REGION: &str = "us-west-2";

/// The space provisioned to the pool these tests allocate resource ranges out of.
const POOL_SPACE: &str = "10.200.0.0/16";

/// How many addresses of a subnet's range AWS reserves, so none of them is available to a network
/// interface (Amazon VPC User Guide, "Subnet CIDR blocks"), which is what makes an empty subnet's
/// IP usage non-zero.
const SUBNET_RESERVED_ADDRESSES: f64 = 5.0;

/// One call of `action` signed for [`REGION`].
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One member of a response body, or a panic naming the case.
fn minted(case: &str, body: &str, name: &str) -> String {
    super::ec2_query::element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The IPAM every test here starts from, with its two default scopes.
struct Fixture {
    router: MultiServiceRouter,
    ipam_id: String,
    private_scope_id: String,
    public_scope_id: String,
}

/// An IPAM created through `CreateIpam`, operating in [`REGION`] alone.
async fn fixture(case: &str) -> Fixture {
    let (router, _engine) = super::ec2_query::emulator();
    let body = ok(case, &router, "CreateIpam", &[]).await;
    Fixture {
        ipam_id: minted(case, &body, "ipamId"),
        private_scope_id: minted(case, &body, "privateDefaultScopeId"),
        public_scope_id: minted(case, &body, "publicDefaultScopeId"),
        router,
    }
}

/// An IPv4 pool of `scope_id` with [`POOL_SPACE`] provisioned to it, allocating in [`REGION`], as
/// its `IpamPoolId`.
async fn pool_with_space(case: &str, router: &MultiServiceRouter, scope_id: &str) -> String {
    let created = ok(
        case,
        router,
        "CreateIpamPool",
        &[
            param("IpamScopeId", scope_id),
            param("AddressFamily", ec2::state::ADDRESS_FAMILY_IPV4),
            param("Locale", REGION),
        ],
    )
    .await;
    let pool_id = minted(case, &created, "ipamPoolId");
    ok(
        case,
        router,
        "ProvisionIpamPoolCidr",
        &[param("IpamPoolId", &pool_id), param("Cidr", POOL_SPACE)],
    )
    .await;
    pool_id
}

/// A VPC with `cidr_block`, out of `pool_id` when one is named, carrying `tags`, as its `VpcId`.
async fn create_vpc(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    cidr_block: &str,
    pool_id: Option<&str>,
    tags: &[(&str, &str)],
) -> String {
    let mut params = vec![param("CidrBlock", cidr_block)];
    if let Some(pool_id) = pool_id {
        params.push(param("Ipv4IpamPoolId", pool_id));
    }
    params.extend(super::ec2_query::tag_specification("vpc", tags));
    let (status, body) = call(router, region, "CreateVpc", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVpc, got {status} with body {body}"
    );
    minted(case, &body, "vpcId")
}

/// A subnet of `vpc_id` with `cidr_block`, as its `SubnetId`.
async fn create_subnet(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    cidr_block: &str,
) -> String {
    let body = ok(
        case,
        router,
        "CreateSubnet",
        &[param("VpcId", vpc_id), param("CidrBlock", cidr_block)],
    )
    .await;
    minted(case, &body, "subnetId")
}

/// One resource CIDR as a response reports it, every member read off the wire.
#[derive(Debug, Clone, PartialEq)]
struct ReportedResourceCidr {
    ipam_id: Option<String>,
    ipam_scope_id: Option<String>,
    ipam_pool_id: Option<String>,
    resource_id: Option<String>,
    resource_type: Option<String>,
    resource_cidr: Option<String>,
    resource_region: Option<String>,
    resource_owner_id: Option<String>,
    resource_name: Option<String>,
    management_state: Option<String>,
    compliance_status: Option<String>,
    overlap_status: Option<String>,
    ip_usage: Option<String>,
    vpc_id: Option<String>,
    availability_zone_id: Option<String>,
    resource_tags: Vec<(String, String)>,
}

/// One reported resource CIDR read off the element `node`.
fn read_resource_cidr(node: roxmltree::Node<'_, '_>) -> ReportedResourceCidr {
    ReportedResourceCidr {
        ipam_id: child_text(node, "ipamId"),
        ipam_scope_id: child_text(node, "ipamScopeId"),
        ipam_pool_id: child_text(node, "ipamPoolId"),
        resource_id: child_text(node, "resourceId"),
        resource_type: child_text(node, "resourceType"),
        resource_cidr: child_text(node, "resourceCidr"),
        resource_region: child_text(node, "resourceRegion"),
        resource_owner_id: child_text(node, "resourceOwnerId"),
        resource_name: child_text(node, "resourceName"),
        management_state: child_text(node, "managementState"),
        compliance_status: child_text(node, "complianceStatus"),
        overlap_status: child_text(node, "overlapStatus"),
        ip_usage: child_text(node, "ipUsage"),
        vpc_id: child_text(node, "vpcId"),
        availability_zone_id: child_text(node, "availabilityZoneId"),
        resource_tags: set_items(node, "resourceTagSet")
            .into_iter()
            .map(|tag| {
                (
                    child_text(tag, "key").unwrap_or_default(),
                    child_text(tag, "value").unwrap_or_default(),
                )
            })
            .collect(),
    }
}

/// The resource CIDRs a `GetIpamResourceCidrs` carrying `params` reports, and the `NextToken` it
/// answered with.
async fn get_resource_cidrs(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
) -> (Vec<ReportedResourceCidr>, Option<String>) {
    let body = ok(case, router, "GetIpamResourceCidrs", params).await;
    let parsed = document(case, &body);
    let root = parsed.root_element();
    let reported = set_items(root, "ipamResourceCidrSet")
        .into_iter()
        .map(read_resource_cidr)
        .collect();
    (reported, super::ec2_query::member(&body, "nextToken"))
}

/// The resource CIDRs of `scope_id`, in the order the service reports them.
async fn resource_cidrs_of(
    case: &str,
    router: &MultiServiceRouter,
    scope_id: &str,
) -> Vec<ReportedResourceCidr> {
    get_resource_cidrs(case, router, &[param("IpamScopeId", scope_id)])
        .await
        .0
}

/// The one resource CIDR of `scope_id` for `resource_id`, or a panic naming the case.
async fn resource_cidr_of(
    case: &str,
    router: &MultiServiceRouter,
    scope_id: &str,
    resource_id: &str,
) -> ReportedResourceCidr {
    let reported = resource_cidrs_of(case, router, scope_id).await;
    reported
        .iter()
        .find(|row| row.resource_id.as_deref() == Some(resource_id))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a resource CIDR for {resource_id} in {scope_id}, got \
                 {reported:?}"
            )
        })
}

/// The `(resource_id, resource_cidr)` pairs of `reported`, which is what a narrowing assertion
/// compares: they name one row each.
fn rows_of(reported: &[ReportedResourceCidr]) -> Vec<(String, String)> {
    reported
        .iter()
        .map(|row| {
            (
                row.resource_id.clone().unwrap_or_default(),
                row.resource_cidr.clone().unwrap_or_default(),
            )
        })
        .collect()
}

/// The form fields of a `ModifyIpamResourceCidr` naming one resource CIDR.
fn modify_params(
    scope_id: &str,
    resource_id: &str,
    resource_cidr: &str,
    monitored: bool,
) -> Vec<Param> {
    vec![
        param("CurrentIpamScopeId", scope_id),
        param("ResourceId", resource_id),
        param("ResourceCidr", resource_cidr),
        param("ResourceRegion", REGION),
        param("Monitored", if monitored { "true" } else { "false" }),
    ]
}

/// The resource CIDR a `ModifyIpamResourceCidr` carrying `params` answered with.
async fn modify_resource_cidr(
    case: &str,
    router: &MultiServiceRouter,
    params: &[Param],
) -> ReportedResourceCidr {
    let body = ok(case, router, "ModifyIpamResourceCidr", params).await;
    let parsed = document(case, &body);
    let node = child(parsed.root_element(), "ipamResourceCidr").unwrap_or_else(|| {
        panic!("case {case:?}: no <ipamResourceCidr> in {body}")
    });
    read_resource_cidr(node)
}

/// Every allocation `GetIpamPoolAllocations` reports for `pool_id`, as `(resourceId, cidr)`.
async fn allocations(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
) -> Vec<(String, String)> {
    let body = ok(
        case,
        router,
        "GetIpamPoolAllocations",
        &[param("IpamPoolId", pool_id)],
    )
    .await;
    let parsed = document(case, &body);
    let mut reported: Vec<(String, String)> = parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolAllocationId").is_some())
        .map(|node| {
            (
                child_text(node, "resourceId").unwrap_or_default(),
                child_text(node, "cidr").unwrap_or_default(),
            )
        })
        .collect();
    reported.sort();
    reported
}

/// The IP usage a response reported, as the number it names.
fn ip_usage(case: &str, row: &ReportedResourceCidr) -> f64 {
    let reported = row
        .ip_usage
        .as_deref()
        .unwrap_or_else(|| panic!("case {case:?}: no <ipUsage> on {row:?}"));
    reported
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: <ipUsage> {reported:?} must be a number"))
}

/// Assert that two IP usages are the same number, within the slack a decimal rendering leaves.
fn assert_usage(case: &str, actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "case {case:?}: expected an IP usage of {expected}, got {actual}"
    );
}

/// Every `(resource_id, cidr)` pair the account's VPCs and subnets of `region` hold, read through
/// `DescribeVpcs` and `DescribeSubnets` — the resources IPAM monitors, as the operations that OWN
/// them report them.
///
/// The independent statement of what a scope's resource CIDRs are: the expectation is built from
/// the resource reports rather than from another `GetIpamResourceCidrs`, so a derivation that
/// invented or dropped a range would show up here. It covers the Region's DEFAULT VPC and its
/// subnets too, which the account holds without anything here creating them.
async fn vpc_and_subnet_ranges(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
) -> Vec<(String, String)> {
    let mut ranges: Vec<(String, String)> = Vec::new();
    let (status, body) = call(router, region, "DescribeVpcs", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeVpcs, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    for vpc in parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "vpcId").is_some())
    {
        let vpc_id = child_text(vpc, "vpcId").unwrap_or_default();
        for association in set_items(vpc, "cidrBlockAssociationSet") {
            let held = child(association, "cidrBlockState")
                .and_then(|state| child_text(state, "state"))
                .unwrap_or_default();
            if held != "associated" {
                continue;
            }
            ranges.push((
                vpc_id.clone(),
                child_text(association, "cidrBlock").unwrap_or_default(),
            ));
        }
    }
    let (status, body) = call(router, region, "DescribeSubnets", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeSubnets, got {status} with body {body}"
    );
    let parsed = document(case, &body);
    for subnet in parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "subnetId").is_some())
    {
        ranges.push((
            child_text(subnet, "subnetId").unwrap_or_default(),
            child_text(subnet, "cidrBlock").unwrap_or_default(),
        ));
    }
    ranges.sort();
    ranges
}

/// Every resource of the account in a Region the scope's IPAM operates in is a resource CIDR of
/// that scope, with the members of each row read off the resource the store holds.
#[tokio::test]
async fn the_scope_reports_the_ipv4_ranges_of_its_ipam_s_vpcs_and_subnets() {
    let case = "vpcs and subnets of the operating Region";
    let fixture = fixture(case).await;
    let router = &fixture.router;
    let pool_id = pool_with_space(case, router, &fixture.private_scope_id).await;

    // A VPC whose range came OUT of the pool, with a subnet taking a quarter of it.
    let from_pool = create_vpc(
        case,
        router,
        REGION,
        "10.200.1.0/24",
        Some(&pool_id),
        &[("Name", "from-the-pool")],
    )
    .await;
    let subnet = create_subnet(case, router, &from_pool, "10.200.1.0/26").await;
    // A VPC on a range no pool hands out.
    let outside_pool = create_vpc(case, router, REGION, "10.60.0.0/16", None, &[]).await;
    // And one in a Region this IPAM does not operate in, which it monitors nothing of.
    let elsewhere = create_vpc(case, router, UNMONITORED_REGION, "10.70.0.0/16", None, &[]).await;

    let reported = resource_cidrs_of(case, router, &fixture.private_scope_id).await;
    assert_eq!(
        rows_of(&reported),
        vpc_and_subnet_ranges(case, router, REGION).await,
        "case {case:?}: the scope's resource CIDRs must be exactly the IPv4 ranges the account's \
         VPCs and subnets of {REGION} hold, got {reported:?}"
    );
    assert!(
        !rows_of(&reported)
            .iter()
            .any(|(resource_id, _)| resource_id == &elsewhere),
        "case {case:?}: expected nothing of {UNMONITORED_REGION} ({elsewhere}) in the report, got \
         {reported:?}"
    );
    assert!(
        reported
            .iter()
            .all(|row| row.resource_region.as_deref() == Some(REGION)),
        "case {case:?}: every reported resource CIDR must be in a Region the IPAM operates in, \
         got {reported:?}"
    );
    for created in [&from_pool, &outside_pool, &subnet] {
        assert!(
            reported
                .iter()
                .any(|row| row.resource_id.as_deref() == Some(created.as_str())),
            "case {case:?}: expected {created} among the scope's resource CIDRs, got {reported:?}"
        );
    }

    let account = ::engine::emulator_account_id().to_string();
    let managed = resource_cidr_of(case, router, &fixture.private_scope_id, &from_pool).await;
    assert_eq!(
        (
            managed.ipam_id.as_deref(),
            managed.ipam_scope_id.as_deref(),
            managed.ipam_pool_id.as_deref(),
            managed.resource_type.as_deref(),
            managed.resource_region.as_deref(),
            managed.resource_owner_id.as_deref(),
            managed.resource_name.as_deref(),
            managed.vpc_id.as_deref(),
            managed.availability_zone_id.as_deref(),
            managed.management_state.as_deref(),
            managed.compliance_status.as_deref(),
            managed.overlap_status.as_deref(),
        ),
        (
            Some(fixture.ipam_id.as_str()),
            Some(fixture.private_scope_id.as_str()),
            Some(pool_id.as_str()),
            Some(IPAM_RESOURCE_TYPE_VPC),
            Some(REGION),
            Some(account.as_str()),
            Some("from-the-pool"),
            Some(from_pool.as_str()),
            None,
            Some(IPAM_MANAGEMENT_STATE_MANAGED),
            Some(IPAM_COMPLIANCE_STATUS_COMPLIANT),
            Some(IPAM_OVERLAP_STATUS_NONOVERLAPPING),
        ),
        "case {case:?}: the VPC out of the pool is reported wrong: {managed:?}"
    );
    assert_eq!(
        managed.resource_tags,
        vec![("Name".to_string(), "from-the-pool".to_string())],
        "case {case:?}: expected the VPC's own tags on its resource CIDR, got {managed:?}"
    );
    // "For resources that are VPCs, this is the percentage of IP address space in the VPC that's
    // taken up by subnet CIDRs" (`IpamResourceCidr$IpUsage`): one /26 of a /24.
    assert_usage(case, ip_usage(case, &managed), 64.0 / 256.0);

    let unmanaged = resource_cidr_of(case, router, &fixture.private_scope_id, &outside_pool).await;
    assert_eq!(
        (
            unmanaged.ipam_pool_id.as_deref(),
            unmanaged.management_state.as_deref(),
            unmanaged.compliance_status.as_deref(),
            unmanaged.resource_name.as_deref(),
        ),
        (
            None,
            Some(IPAM_MANAGEMENT_STATE_UNMANAGED),
            Some(IPAM_COMPLIANCE_STATUS_UNMANAGED),
            None,
        ),
        "case {case:?}: a VPC on a range no pool hands out is reported wrong: {unmanaged:?}"
    );
    assert_usage(case, ip_usage(case, &unmanaged), 0.0);

    let subnet_row = resource_cidr_of(case, router, &fixture.private_scope_id, &subnet).await;
    assert_eq!(
        (
            subnet_row.resource_type.as_deref(),
            subnet_row.vpc_id.as_deref(),
            subnet_row.management_state.as_deref(),
            subnet_row.overlap_status.as_deref(),
        ),
        (
            Some(IPAM_RESOURCE_TYPE_SUBNET),
            Some(from_pool.as_str()),
            Some(IPAM_MANAGEMENT_STATE_UNMANAGED),
            // Inside its own VPC's range by construction, which is not an overlap.
            Some(IPAM_OVERLAP_STATUS_NONOVERLAPPING),
        ),
        "case {case:?}: the subnet's range is reported wrong: {subnet_row:?}"
    );
    assert!(
        subnet_row
            .availability_zone_id
            .as_deref()
            .is_some_and(|zone| !zone.is_empty()),
        "case {case:?}: expected the subnet's Availability Zone ID, got {subnet_row:?}"
    );
    // "For resources that are subnets, … this is the percentage of IPv4 address space in the subnet
    // that's in use" (`IpamResourceCidr$IpUsage`): the addresses of the /26 no interface may take.
    assert_usage(case, ip_usage(case, &subnet_row), SUBNET_RESERVED_ADDRESSES / 64.0);

    // The scope of the OTHER type holds none of these ranges: they are private space.
    let public = resource_cidrs_of(case, router, &fixture.public_scope_id).await;
    assert!(
        public.is_empty(),
        "case {case:?}: expected no resource CIDR in the public scope, got {public:?}"
    );
}

/// What a row of the table [`management_and_compliance_follow_the_pool_that_hands_out_the_range`]
/// drives: what happens to the pool, or to the monitoring, after the VPC takes its range.
#[derive(Debug, Clone, Copy)]
enum AfterTheAllocation {
    /// Nothing: the range still meets the rules it was handed out under.
    Nothing,
    /// The pool is given a minimum netmask length the range is too LARGE for.
    TightenTheNetmaskRules,
    /// The pool is made to require a tag the resource does not carry.
    RequireATagTheResourceLacks,
    /// The range never came out of a pool at all.
    NoPoolAtAll,
    /// IPAM is told to stop monitoring the range.
    StopMonitoring,
}

/// The management state of a resource CIDR follows the pool of the scope that hands out its space,
/// and its compliance follows that pool's allocation rules as they stand.
#[tokio::test]
async fn management_and_compliance_follow_the_pool_that_hands_out_the_range() {
    struct Row {
        case: &'static str,
        after: AfterTheAllocation,
        management_state: &'static str,
        compliance_status: &'static str,
        in_a_pool: bool,
    }

    let rows = [
        Row {
            case: "a range still within the pool's rules",
            after: AfterTheAllocation::Nothing,
            management_state: IPAM_MANAGEMENT_STATE_MANAGED,
            compliance_status: IPAM_COMPLIANCE_STATUS_COMPLIANT,
            in_a_pool: true,
        },
        Row {
            case: "a range too large for the pool's netmask rules",
            after: AfterTheAllocation::TightenTheNetmaskRules,
            management_state: IPAM_MANAGEMENT_STATE_MANAGED,
            compliance_status: IPAM_COMPLIANCE_STATUS_NONCOMPLIANT,
            in_a_pool: true,
        },
        Row {
            case: "a resource missing a tag the pool requires",
            after: AfterTheAllocation::RequireATagTheResourceLacks,
            management_state: IPAM_MANAGEMENT_STATE_MANAGED,
            compliance_status: IPAM_COMPLIANCE_STATUS_NONCOMPLIANT,
            in_a_pool: true,
        },
        Row {
            case: "a range no pool hands out",
            after: AfterTheAllocation::NoPoolAtAll,
            management_state: IPAM_MANAGEMENT_STATE_UNMANAGED,
            compliance_status: IPAM_COMPLIANCE_STATUS_UNMANAGED,
            in_a_pool: false,
        },
        Row {
            case: "a range IPAM was told to stop monitoring",
            after: AfterTheAllocation::StopMonitoring,
            management_state: IPAM_MANAGEMENT_STATE_IGNORED,
            compliance_status: IPAM_COMPLIANCE_STATUS_IGNORED,
            in_a_pool: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let fixture = fixture(case).await;
        let router = &fixture.router;
        let pool_id = pool_with_space(case, router, &fixture.private_scope_id).await;
        let from_pool = !matches!(row.after, AfterTheAllocation::NoPoolAtAll);
        let cidr = "10.200.2.0/24";
        let vpc_id = create_vpc(
            case,
            router,
            REGION,
            cidr,
            from_pool.then_some(pool_id.as_str()),
            &[],
        )
        .await;
        match row.after {
            AfterTheAllocation::Nothing | AfterTheAllocation::NoPoolAtAll => {}
            AfterTheAllocation::TightenTheNetmaskRules => {
                ok(
                    case,
                    router,
                    "ModifyIpamPool",
                    &[
                        param("IpamPoolId", &pool_id),
                        param("AllocationMinNetmaskLength", "25"),
                    ],
                )
                .await;
            }
            AfterTheAllocation::RequireATagTheResourceLacks => {
                ok(
                    case,
                    router,
                    "ModifyIpamPool",
                    &[
                        param("IpamPoolId", &pool_id),
                        param("AddAllocationResourceTag.1.Key", "Owner"),
                        param("AddAllocationResourceTag.1.Value", "TeamA"),
                    ],
                )
                .await;
            }
            AfterTheAllocation::StopMonitoring => {
                modify_resource_cidr(
                    case,
                    router,
                    &modify_params(&fixture.private_scope_id, &vpc_id, cidr, false),
                )
                .await;
            }
        }

        let reported = resource_cidr_of(case, router, &fixture.private_scope_id, &vpc_id).await;
        assert_eq!(
            (
                reported.management_state.as_deref(),
                reported.compliance_status.as_deref(),
                reported.ipam_pool_id.is_some(),
            ),
            (
                Some(row.management_state),
                Some(row.compliance_status),
                row.in_a_pool,
            ),
            "case {case:?}: expected {state}/{compliance} with in_a_pool={in_a_pool}, got \
             {reported:?}",
            state = row.management_state,
            compliance = row.compliance_status,
            in_a_pool = row.in_a_pool,
        );
    }
}

/// Two ranges of one scope that share an address are both overlapping; one IPAM does not monitor is
/// neither, and makes no other range overlapping either.
#[tokio::test]
async fn ranges_that_share_addresses_in_one_scope_are_reported_as_overlapping() {
    struct Row {
        case: &'static str,
        left: &'static str,
        right: &'static str,
        overlapping: bool,
    }

    let rows = [
        Row {
            case: "the same range twice",
            left: "10.80.0.0/16",
            right: "10.80.0.0/16",
            overlapping: true,
        },
        Row {
            case: "one range inside the other",
            left: "10.81.0.0/16",
            right: "10.81.3.0/24",
            overlapping: true,
        },
        Row {
            case: "ranges that meet at no address",
            left: "10.82.0.0/16",
            right: "10.83.0.0/16",
            overlapping: false,
        },
        Row {
            case: "ranges one address apart",
            left: "10.84.0.0/24",
            right: "10.84.1.0/24",
            overlapping: false,
        },
    ];

    for row in rows {
        let case = row.case;
        let fixture = fixture(case).await;
        let router = &fixture.router;
        let left = create_vpc(case, router, REGION, row.left, None, &[]).await;
        let right = create_vpc(case, router, REGION, row.right, None, &[]).await;
        let expected = if row.overlapping {
            IPAM_OVERLAP_STATUS_OVERLAPPING
        } else {
            IPAM_OVERLAP_STATUS_NONOVERLAPPING
        };
        for vpc_id in [&left, &right] {
            let reported = resource_cidr_of(case, router, &fixture.private_scope_id, vpc_id).await;
            assert_eq!(
                reported.overlap_status.as_deref(),
                Some(expected),
                "case {case:?}: expected {expected} for {vpc_id}, got {reported:?}"
            );
        }

        // A range IPAM stops monitoring is not tracked for overlap, so it is `ignored` and the
        // range it used to meet is nonoverlapping again.
        modify_resource_cidr(
            case,
            router,
            &modify_params(&fixture.private_scope_id, &left, row.left, false),
        )
        .await;
        let ignored = resource_cidr_of(case, router, &fixture.private_scope_id, &left).await;
        assert_eq!(
            ignored.overlap_status.as_deref(),
            Some(IPAM_OVERLAP_STATUS_IGNORED),
            "case {case:?}: expected an ignored overlap status for {left}, got {ignored:?}"
        );
        let other = resource_cidr_of(case, router, &fixture.private_scope_id, &right).await;
        assert_eq!(
            other.overlap_status.as_deref(),
            Some(IPAM_OVERLAP_STATUS_NONOVERLAPPING),
            "case {case:?}: expected {right} to meet no monitored range once {left} is ignored, \
             got {other:?}"
        );
    }
}

/// Switching the monitoring off gives the pool its space back, and switching it on again leaves the
/// range monitored but in no pool.
#[tokio::test]
async fn un_monitoring_a_resource_cidr_releases_the_pool_allocation_that_held_its_range() {
    let case = "monitoring switched off and on again";
    let fixture = fixture(case).await;
    let router = &fixture.router;
    let pool_id = pool_with_space(case, router, &fixture.private_scope_id).await;
    let cidr = "10.200.4.0/24";
    let vpc_id = create_vpc(case, router, REGION, cidr, Some(&pool_id), &[]).await;

    assert_eq!(
        allocations(case, router, &pool_id).await,
        vec![(vpc_id.clone(), cidr.to_string())],
        "case {case:?}: expected the pool to hold the VPC's range before anything is modified"
    );

    let answered = modify_resource_cidr(
        case,
        router,
        &modify_params(&fixture.private_scope_id, &vpc_id, cidr, false),
    )
    .await;
    assert_eq!(
        (
            answered.management_state.as_deref(),
            answered.compliance_status.as_deref(),
            answered.overlap_status.as_deref(),
            answered.ipam_pool_id.as_deref(),
        ),
        (
            Some(IPAM_MANAGEMENT_STATE_IGNORED),
            Some(IPAM_COMPLIANCE_STATUS_IGNORED),
            Some(IPAM_OVERLAP_STATUS_IGNORED),
            None,
        ),
        "case {case:?}: the modification answered the wrong resource CIDR: {answered:?}"
    );
    // What the modification answered is what the report carries: one derivation, two readers.
    assert_eq!(
        resource_cidr_of(case, router, &fixture.private_scope_id, &vpc_id).await,
        answered,
        "case {case:?}: the report and the modification disagree about the resource CIDR"
    );
    assert!(
        allocations(case, router, &pool_id).await.is_empty(),
        "case {case:?}: expected the pool to have given the VPC's range back"
    );

    let monitored_again = modify_resource_cidr(
        case,
        router,
        &modify_params(&fixture.private_scope_id, &vpc_id, cidr, true),
    )
    .await;
    assert_eq!(
        (
            monitored_again.management_state.as_deref(),
            monitored_again.compliance_status.as_deref(),
            monitored_again.overlap_status.as_deref(),
            monitored_again.ipam_pool_id.as_deref(),
        ),
        (
            Some(IPAM_MANAGEMENT_STATE_UNMANAGED),
            Some(IPAM_COMPLIANCE_STATUS_UNMANAGED),
            Some(IPAM_OVERLAP_STATUS_NONOVERLAPPING),
            None,
        ),
        "case {case:?}: monitoring a range again does not put it back in a pool: \
         {monitored_again:?}"
    );
    assert!(
        allocations(case, router, &pool_id).await.is_empty(),
        "case {case:?}: expected the released range to stay free space of the pool"
    );
}

/// A resource CIDR transferred to another private scope of the same IPAM leaves the scope it was in.
#[tokio::test]
async fn a_resource_cidr_transferred_to_another_private_scope_leaves_the_one_it_was_in() {
    let case = "transferred between scopes";
    let fixture = fixture(case).await;
    let router = &fixture.router;
    let created = ok(
        case,
        router,
        "CreateIpamScope",
        &[
            param("IpamId", &fixture.ipam_id),
            param("Description", "a scope to transfer a resource CIDR into"),
        ],
    )
    .await;
    let destination = minted(case, &created, "ipamScopeId");
    let cidr = "10.90.0.0/16";
    let vpc_id = create_vpc(case, router, REGION, cidr, None, &[]).await;

    let mut params = modify_params(&fixture.private_scope_id, &vpc_id, cidr, true);
    params.push(param("DestinationIpamScopeId", &destination));
    let answered = modify_resource_cidr(case, router, &params).await;
    assert_eq!(
        answered.ipam_scope_id.as_deref(),
        Some(destination.as_str()),
        "case {case:?}: expected the transferred resource CIDR to report the destination scope, \
         got {answered:?}"
    );

    let left_behind = resource_cidrs_of(case, router, &fixture.private_scope_id).await;
    assert!(
        !left_behind
            .iter()
            .any(|row| row.resource_id.as_deref() == Some(vpc_id.as_str())),
        "case {case:?}: expected {vpc_id} to have left {scope}, got {left_behind:?}",
        scope = fixture.private_scope_id
    );
    let transferred = resource_cidr_of(case, router, &destination, &vpc_id).await;
    assert_eq!(
        (
            transferred.resource_cidr.as_deref(),
            transferred.management_state.as_deref(),
        ),
        (Some(cidr), Some(IPAM_MANAGEMENT_STATE_UNMANAGED)),
        "case {case:?}: the transferred resource CIDR is reported wrong: {transferred:?}"
    );

    // And back again: the destination scope is the one the request names, whichever direction the
    // transfer goes in.
    let mut back = modify_params(&destination, &vpc_id, cidr, true);
    back.push(param("DestinationIpamScopeId", &fixture.private_scope_id));
    let returned = modify_resource_cidr(case, router, &back).await;
    assert_eq!(
        returned.ipam_scope_id.as_deref(),
        Some(fixture.private_scope_id.as_str()),
        "case {case:?}: expected the resource CIDR back in the default scope, got {returned:?}"
    );
}

/// Every selector and documented filter of the report narrows it, and `MaxResults` pages it.
#[tokio::test]
async fn the_report_narrows_by_its_selectors_and_filters_and_pages_by_max_results() {
    let case = "narrowing and paging";
    let fixture = fixture(case).await;
    let router = &fixture.router;
    let scope = fixture.private_scope_id.clone();
    let pool_id = pool_with_space(case, router, &scope).await;
    let managed = create_vpc(
        case,
        router,
        REGION,
        "10.200.5.0/24",
        Some(&pool_id),
        &[("Tier", "web")],
    )
    .await;
    let plain = create_vpc(case, router, REGION, "10.91.0.0/16", None, &[]).await;
    let subnet = create_subnet(case, router, &managed, "10.200.5.0/26").await;
    let account = ::engine::emulator_account_id().to_string();

    // The whole report, which every narrowing below is a SUBSET of: the rows the account's
    // resources of this Region hold, the Region's default VPC and its subnets among them.
    let every = resource_cidrs_of(case, router, &scope).await;

    /// One narrowing: the members it adds to the request, and which rows of the whole report it
    /// names — stated over the reported row, so the expectation is a statement about the set and
    /// not a second copy of the handler's selector.
    struct Narrowing<'a> {
        case: &'static str,
        params: Vec<Param>,
        holds: Box<dyn Fn(&ReportedResourceCidr) -> bool + 'a>,
    }

    let carries = |row: &ReportedResourceCidr, key: &str, value: Option<&str>| {
        row.resource_tags.iter().any(|(held_key, held_value)| {
            held_key == key && value.is_none_or(|value| held_value == value)
        })
    };
    let narrowings = [
        Narrowing {
            case: "nothing named at all",
            params: vec![],
            holds: Box::new(|_| true),
        },
        Narrowing {
            case: "the resource named by id",
            params: vec![param("ResourceId", &plain)],
            holds: Box::new(|row| row.resource_id.as_deref() == Some(plain.as_str())),
        },
        Narrowing {
            case: "the resource type",
            params: vec![param("ResourceType", IPAM_RESOURCE_TYPE_SUBNET)],
            holds: Box::new(|row| row.resource_type.as_deref() == Some(IPAM_RESOURCE_TYPE_SUBNET)),
        },
        Narrowing {
            case: "the pool holding the range",
            params: vec![param("IpamPoolId", &pool_id)],
            holds: Box::new(|row| row.ipam_pool_id.as_deref() == Some(pool_id.as_str())),
        },
        Narrowing {
            case: "the account owning the resources",
            params: vec![param("ResourceOwner", &account)],
            holds: Box::new(|row| row.resource_owner_id.as_deref() == Some(account.as_str())),
        },
        Narrowing {
            case: "a tag key and value the resource carries",
            params: vec![
                param("ResourceTag.Key", "Tier"),
                param("ResourceTag.Value", "web"),
            ],
            holds: Box::new(|row| carries(row, "Tier", Some("web"))),
        },
        Narrowing {
            case: "a tag key alone, whatever the value",
            params: vec![param("ResourceTag.Key", "Tier")],
            holds: Box::new(|row| carries(row, "Tier", None)),
        },
        Narrowing {
            case: "a tag no resource carries",
            params: vec![
                param("ResourceTag.Key", "Tier"),
                param("ResourceTag.Value", "db"),
            ],
            holds: Box::new(|row| carries(row, "Tier", Some("db"))),
        },
        Narrowing {
            case: "the management-state filter",
            params: super::ec2_query::filters(&[(
                "management-state",
                &[IPAM_MANAGEMENT_STATE_MANAGED],
            )]),
            holds: Box::new(|row| {
                row.management_state.as_deref() == Some(IPAM_MANAGEMENT_STATE_MANAGED)
            }),
        },
        Narrowing {
            case: "the resource-cidr filter",
            params: super::ec2_query::filters(&[("resource-cidr", &["10.91.0.0/16"])]),
            holds: Box::new(|row| row.resource_cidr.as_deref() == Some("10.91.0.0/16")),
        },
        Narrowing {
            case: "the vpc-id filter, which a subnet carries too",
            params: super::ec2_query::filters(&[("vpc-id", &[managed.as_str()])]),
            holds: Box::new(|row| row.vpc_id.as_deref() == Some(managed.as_str())),
        },
        Narrowing {
            case: "the resource-region filter naming another Region",
            params: super::ec2_query::filters(&[("resource-region", &[UNMONITORED_REGION])]),
            holds: Box::new(|row| row.resource_region.as_deref() == Some(UNMONITORED_REGION)),
        },
        Narrowing {
            case: "a filter and a selector together, joined with AND",
            params: {
                let mut params = vec![param("ResourceType", IPAM_RESOURCE_TYPE_VPC)];
                params.extend(super::ec2_query::filters(&[(
                    "compliance-status",
                    &[IPAM_COMPLIANCE_STATUS_UNMANAGED],
                )]));
                params
            },
            holds: Box::new(|row| {
                row.resource_type.as_deref() == Some(IPAM_RESOURCE_TYPE_VPC)
                    && row.compliance_status.as_deref() == Some(IPAM_COMPLIANCE_STATUS_UNMANAGED)
            }),
        },
    ];

    for narrowing in narrowings {
        let mut params = vec![param("IpamScopeId", &scope)];
        params.extend(narrowing.params);
        let (reported, _) = get_resource_cidrs(narrowing.case, router, &params).await;
        let expected: Vec<(String, String)> = rows_of(
            &every
                .iter()
                .filter(|row| (narrowing.holds)(row))
                .cloned()
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            rows_of(&reported),
            expected,
            "case {case:?}: expected {expected:?} for {narrowing_case:?}, got {reported:?}",
            narrowing_case = narrowing.case
        );
    }

    // Each narrowing above has to be a REAL narrowing of a report that holds several rows, or the
    // table would pass on an operation that reports nothing at all.
    assert!(
        every.len() > 3,
        "case {case:?}: expected the scope to hold several resource CIDRs, got {every:?}"
    );
    let kinds: BTreeSet<Option<&str>> = every
        .iter()
        .map(|row| row.resource_type.as_deref())
        .collect();
    assert_eq!(
        kinds,
        [Some(IPAM_RESOURCE_TYPE_SUBNET), Some(IPAM_RESOURCE_TYPE_VPC)]
            .into_iter()
            .collect::<BTreeSet<_>>(),
        "case {case:?}: expected both kinds of monitored resource in the report, got {every:?}"
    );
    assert!(
        every
            .iter()
            .any(|row| row.resource_id.as_deref() == Some(subnet.as_str())),
        "case {case:?}: expected the subnet created here in the report, got {every:?}"
    );

    // Paging: the model's `MaxResults` starts at 5, so the walk takes a page of that size over a
    // report that holds more than five rows, and has to cover it exactly once.
    for cidr in ["10.101.0.0/16", "10.102.0.0/16", "10.103.0.0/16"] {
        create_vpc(case, router, REGION, cidr, None, &[]).await;
    }
    let every = resource_cidrs_of(case, router, &scope).await;
    assert!(
        every.len() > 5,
        "case {case:?}: expected more resource CIDRs than one page holds, got {every:?}"
    );
    let page_size = 5;
    let mut walked: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    let mut ended = false;
    // Bounded: a token that did not advance would otherwise loop for ever, and the walk cannot take
    // more calls than there are rows.
    for _ in 0..=every.len() {
        let mut params = vec![
            param("IpamScopeId", &scope),
            param("MaxResults", &page_size.to_string()),
        ];
        if let Some(token) = token.as_deref() {
            params.push(param("NextToken", token));
        }
        let (page, next) = get_resource_cidrs(case, router, &params).await;
        assert!(
            page.len() <= page_size,
            "case {case:?}: expected at most {page_size} resource CIDRs per page, got {page:?}"
        );
        walked.extend(rows_of(&page));
        token = next;
        if token.is_none() {
            ended = true;
            break;
        }
    }
    assert!(
        ended,
        "case {case:?}: the walk must reach a page that carries no token"
    );
    assert_eq!(
        walked,
        rows_of(&every),
        "case {case:?}: the walk must cover every resource CIDR exactly once, in the report's own \
         order"
    );
}

/// A request naming something the store does not hold, or a member the operation cannot act on, is
/// refused with that cause's own code and sentence — and a dry run reports what would have happened
/// without reading or writing anything.
#[tokio::test]
async fn requests_naming_what_the_store_does_not_hold_are_refused() {
    let case = "refusals";
    let fixture = fixture(case).await;
    let router = &fixture.router;
    let scope = fixture.private_scope_id.clone();
    let pool_id = pool_with_space(case, router, &scope).await;
    let cidr = "10.200.6.0/24";
    let vpc_id = create_vpc(case, router, REGION, cidr, Some(&pool_id), &[]).await;
    let other_ipam = ok(case, router, "CreateIpam", &[]).await;
    let other_scope = minted(case, &other_ipam, "privateDefaultScopeId");

    struct Refusal {
        case: &'static str,
        action: &'static str,
        params: Vec<Param>,
        status: u16,
        code: &'static str,
        message: Option<String>,
    }

    let refusals = [
        Refusal {
            case: "a report naming something that is not a scope id",
            action: "GetIpamResourceCidrs",
            params: vec![param("IpamScopeId", "not-a-scope")],
            status: 400,
            code: INVALID_IPAM_SCOPE_ID_MALFORMED,
            message: None,
        },
        Refusal {
            case: "a report naming a scope the account does not hold",
            action: "GetIpamResourceCidrs",
            params: vec![param("IpamScopeId", "ipam-scope-0123456789abcdef0")],
            status: 400,
            code: INVALID_IPAM_SCOPE_ID_NOT_FOUND,
            message: Some(
                "The IPAM scope (ipam-scope-0123456789abcdef0) does not exist.".to_string(),
            ),
        },
        Refusal {
            case: "a report naming something that is not a pool id",
            action: "GetIpamResourceCidrs",
            params: vec![param("IpamScopeId", &scope), param("IpamPoolId", "pool")],
            status: 400,
            code: INVALID_IPAM_POOL_ID_MALFORMED,
            message: None,
        },
        Refusal {
            case: "a report naming a pool the account does not hold",
            action: "GetIpamResourceCidrs",
            params: vec![
                param("IpamScopeId", &scope),
                param("IpamPoolId", "ipam-pool-0123456789abcdef0"),
            ],
            status: 400,
            code: INVALID_IPAM_POOL_ID_NOT_FOUND,
            message: None,
        },
        Refusal {
            case: "a report naming a filter this operation does not resolve",
            action: "GetIpamResourceCidrs",
            params: {
                let mut params = vec![param("IpamScopeId", &scope)];
                params.extend(super::ec2_query::filters(&[("resource-colour", &["red"])]));
                params
            },
            status: 400,
            code: INVALID_PARAMETER_VALUE,
            message: Some("The filter 'resource-colour' is invalid".to_string()),
        },
        Refusal {
            case: "a report resuming from a token this service did not mint",
            action: "GetIpamResourceCidrs",
            params: vec![param("IpamScopeId", &scope), param("NextToken", "deadbeef")],
            status: 400,
            code: INVALID_PAGINATION_TOKEN,
            message: Some(ec2::state::INVALID_PAGINATION_TOKEN_MESSAGE.to_string()),
        },
        Refusal {
            case: "a report asked as a dry run",
            action: "GetIpamResourceCidrs",
            params: vec![param("IpamScopeId", &scope), param("DryRun", "true")],
            status: ec2::state::DRY_RUN_OPERATION_STATUS,
            code: ec2::state::DRY_RUN_OPERATION,
            message: Some(ec2::state::DRY_RUN_OPERATION_MESSAGE.to_string()),
        },
        Refusal {
            case: "a modification through the scope of the other type",
            action: "ModifyIpamResourceCidr",
            params: modify_params(&fixture.public_scope_id, &vpc_id, cidr, false),
            status: 400,
            code: INVALID_PARAMETER_VALUE,
            message: Some(format!(
                "The scope {scope} is not private.",
                scope = fixture.public_scope_id
            )),
        },
        Refusal {
            case: "a modification naming a range the resource does not hold",
            action: "ModifyIpamResourceCidr",
            params: modify_params(&scope, &vpc_id, "10.250.0.0/24", false),
            status: 400,
            code: INVALID_RESOURCE_CIDR_NOT_FOUND,
            message: Some(ec2::state::RESOURCE_CIDR_NOT_FOUND_MESSAGE.to_string()),
        },
        Refusal {
            case: "a modification naming a resource the account does not hold",
            action: "ModifyIpamResourceCidr",
            params: modify_params(&scope, "vpc-0123456789abcdef0", cidr, false),
            status: 400,
            code: INVALID_RESOURCE_CIDR_NOT_FOUND,
            message: None,
        },
        Refusal {
            case: "a modification naming the Region the resource is not in",
            action: "ModifyIpamResourceCidr",
            params: {
                let mut params = modify_params(&scope, &vpc_id, cidr, false);
                params.retain(|(name, _)| name != "ResourceRegion");
                params.push(param("ResourceRegion", UNMONITORED_REGION));
                params
            },
            status: 400,
            code: INVALID_RESOURCE_CIDR_NOT_FOUND,
            message: None,
        },
        Refusal {
            case: "a modification naming no resource at all",
            action: "ModifyIpamResourceCidr",
            params: {
                let mut params = modify_params(&scope, &vpc_id, cidr, false);
                params.retain(|(name, _)| name != "ResourceId");
                params
            },
            status: 400,
            code: MISSING_PARAMETER,
            message: Some(ec2::state::missing_required_parameter_message("ResourceId")),
        },
        Refusal {
            case: "a transfer to a scope of another IPAM",
            action: "ModifyIpamResourceCidr",
            params: {
                let mut params = modify_params(&scope, &vpc_id, cidr, true);
                params.push(param("DestinationIpamScopeId", &other_scope));
                params
            },
            status: 400,
            code: INVALID_PARAMETER_VALUE,
            message: None,
        },
        Refusal {
            case: "a modification asked as a dry run",
            action: "ModifyIpamResourceCidr",
            params: {
                let mut params = modify_params(&scope, &vpc_id, cidr, false);
                params.push(param("DryRun", "true"));
                params
            },
            status: ec2::state::DRY_RUN_OPERATION_STATUS,
            code: ec2::state::DRY_RUN_OPERATION,
            message: Some(ec2::state::DRY_RUN_OPERATION_MESSAGE.to_string()),
        },
    ];

    for refusal in refusals {
        let (status, body) = invoke(router, refusal.action, &refusal.params).await;
        assert_eq!(
            status, refusal.status,
            "case {refusal_case:?}: expected HTTP {expected} from {action}, got {status} with \
             body {body}",
            refusal_case = refusal.case,
            expected = refusal.status,
            action = refusal.action,
        );
        assert_eq!(
            error_code(&body),
            refusal.code,
            "case {refusal_case:?}: expected {code}, got {body}",
            refusal_case = refusal.case,
            code = refusal.code,
        );
        if let Some(expected) = refusal.message {
            assert_eq!(
                error_message(&body),
                expected,
                "case {refusal_case:?}: expected the sentence {expected:?}, got {body}",
                refusal_case = refusal.case,
            );
        }
    }

    // Neither dry run changed anything, and neither did any refusal: the range is still the pool's
    // and still monitored.
    let unchanged = resource_cidr_of(case, router, &scope, &vpc_id).await;
    assert_eq!(
        (
            unchanged.management_state.as_deref(),
            unchanged.ipam_scope_id.as_deref(),
            unchanged.ipam_pool_id.as_deref(),
        ),
        (
            Some(IPAM_MANAGEMENT_STATE_MANAGED),
            Some(scope.as_str()),
            Some(pool_id.as_str()),
        ),
        "case {case:?}: a refused request must leave the resource CIDR as it was: {unchanged:?}"
    );
    assert_eq!(
        allocations(case, router, &pool_id).await,
        vec![(vpc_id.clone(), cidr.to_string())],
        "case {case:?}: a refused request must leave the pool holding the range"
    );
}
