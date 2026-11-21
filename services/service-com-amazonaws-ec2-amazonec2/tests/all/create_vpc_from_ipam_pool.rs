// SPDX-License-Identifier: CC0-1.0

//! A VPC whose IPv4 CIDR block comes OUT OF AN IPAM POOL, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test is that A VPC'S IPv4 CIDR BLOCK MAY BE ALLOCATED FROM AN IPAM POOL:
//! the request names the pool and says which block it wants or how large a block to take —
//! exactly one of the two, unless the pool itself sets a default allocation netmask length,
//! which supplies the size when the request names neither — and the block then comes out of
//! the space provisioned to that pool, minus everything already assigned out of it, with the
//! pool reporting the VPC as holding it.
//!
//! The rows of each table differ along what that generalises over: whether the request names
//! the block or a size, which size, whether the pool carries a default size, how the request
//! spells the block, and how the block relates to the pool's space and to what is already
//! assigned. The block the SERVICE chooses is asserted as a property — inside the pool's
//! space, of the asked-for size, overlapping nothing already handed out — rather than as fixed
//! text: which free block it picks is its own choice.
//!
//! Each VPC is read back through `DescribeVpcs` and each allocation through
//! `GetIpamPoolAllocations`, so what the create answered is what the store holds.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// Another Region of the same partition, given to an IPAM as an operating Region so a pool may
/// name it as its locale: a pool that allocates THERE allocates no block to a VPC of
/// [`REGION`].
const OTHER_REGION: &str = "us-west-2";

/// The space provisioned to the pool every test here allocates out of.
const POOL_SPACE: &str = "10.200.0.0/16";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// One IPAM pool with [`POOL_SPACE`] provisioned to it, and `extra` on the pool itself: the
/// router and the pool id.
///
/// The pool's locale is the Region every call here is signed for: "the locale is the Amazon
/// Web Services Region where this IPAM pool is available for allocations"
/// (`AllocateIpamPoolCidr`), so a pool a VPC OF THIS REGION can take space out of has to name
/// it. A pool without one is a row of
/// [`a_pool_that_does_not_allocate_in_this_region_serves_no_vpc`] instead.
async fn pool_with_space(case: &str, extra: &[Param]) -> (MultiServiceRouter, String) {
    let (router, _engine) = super::ec2_query::emulator();
    let ipam = ok(case, &router, "CreateIpam", &[]).await;
    let scope_id = member(case, &ipam, "privateDefaultScopeId");
    let mut params = vec![
        param("IpamScopeId", &scope_id),
        param("AddressFamily", ec2::state::ADDRESS_FAMILY_IPV4),
        param("Locale", REGION),
    ];
    params.extend(extra.iter().cloned());
    let created = ok(case, &router, "CreateIpamPool", &params).await;
    let pool_id = member(case, &created, "ipamPoolId");
    ok(
        case,
        &router,
        "ProvisionIpamPoolCidr",
        &[param("IpamPoolId", &pool_id), param("Cidr", POOL_SPACE)],
    )
    .await;
    (router, pool_id)
}

/// The numeric bounds of `cidr` — the test's own arithmetic, so what the service chose is
/// judged against the pool's space rather than against the service's own helpers.
fn bounds(case: &str, cidr: &str) -> (u32, u32, u8) {
    let (address, prefix_length) = cidr
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: {cidr:?} must be a CIDR block"));
    let prefix_length: u8 = prefix_length
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {cidr:?} must carry a prefix length"));
    let octets: Vec<u32> = address
        .split('.')
        .map(|octet| {
            octet
                .parse()
                .unwrap_or_else(|_| panic!("case {case:?}: {cidr:?} must be an IPv4 block"))
        })
        .collect();
    assert_eq!(
        octets.len(),
        4,
        "case {case:?}: {cidr:?} must be an IPv4 block"
    );
    let base = (octets[0] << 24) | (octets[1] << 16) | (octets[2] << 8) | octets[3];
    let size = 1u32 << (32 - u32::from(prefix_length));
    (base, base + size - 1, prefix_length)
}

/// Every allocation `GetIpamPoolAllocations` reports for `pool_id`, as `(cidr, resourceType,
/// resourceId)` sorted by range.
async fn allocations(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
) -> Vec<(String, String, Option<String>)> {
    let body = ok(
        case,
        router,
        "GetIpamPoolAllocations",
        &[param("IpamPoolId", pool_id)],
    )
    .await;
    let parsed = document(case, &body);
    let mut reported: Vec<(String, String, Option<String>)> = parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolAllocationId").is_some())
        .map(|node| {
            (
                child_text(node, "cidr").unwrap_or_default(),
                child_text(node, "resourceType").unwrap_or_default(),
                child_text(node, "resourceId"),
            )
        })
        .collect();
    reported.sort_by(|left, right| left.0.cmp(&right.0));
    reported
}

/// THE RULE: a create that names an IPAM pool takes the VPC's block out of that pool.
#[tokio::test]
async fn a_vpc_takes_its_cidr_block_out_of_the_pool_the_request_names() {
    // (case, the members beside Ipv4IpamPoolId, the prefix length the VPC must end up with)
    let rows: Vec<(&str, Vec<Param>, u8)> = vec![
        (
            "a /24 the pool chooses",
            vec![param("Ipv4NetmaskLength", "24")],
            24,
        ),
        (
            "a /20 the pool chooses",
            vec![param("Ipv4NetmaskLength", "20")],
            20,
        ),
        (
            "the largest block a VPC may have",
            vec![param(
                "Ipv4NetmaskLength",
                &ec2::state::MIN_VPC_IPV4_PREFIX.to_string(),
            )],
            ec2::state::MIN_VPC_IPV4_PREFIX,
        ),
        (
            "the smallest block a VPC may have",
            vec![param(
                "Ipv4NetmaskLength",
                &ec2::state::MAX_VPC_IPV4_PREFIX.to_string(),
            )],
            ec2::state::MAX_VPC_IPV4_PREFIX,
        ),
        (
            "a block the request names itself",
            vec![param("CidrBlock", "10.200.7.0/24")],
            24,
        ),
        (
            "a block the request names with host bits set",
            vec![param("CidrBlock", "10.200.8.18/24")],
            24,
        ),
    ];

    for (case, members, prefix_length) in rows {
        let (router, pool_id) = pool_with_space(case, &[]).await;
        let mut params = vec![param("Ipv4IpamPoolId", &pool_id)];
        params.extend(members);
        let created = ok(case, &router, "CreateVpc", &params).await;
        let vpc_id = member(case, &created, "vpcId");
        let cidr_block = member(case, &created, "cidrBlock");

        let (first, last, answered_length) = bounds(case, &cidr_block);
        let (space_first, space_last, _) = bounds(case, POOL_SPACE);
        assert_eq!(
            answered_length, prefix_length,
            "case {case:?}: expected a /{prefix_length} block, got {cidr_block:?}"
        );
        assert!(
            first >= space_first && last <= space_last,
            "case {case:?}: expected {cidr_block:?} inside the pool's {POOL_SPACE}, got \
             {first}..={last}"
        );

        // The VPC reads back with that block, and the pool reports the VPC as holding it.
        let described = ok(
            case,
            &router,
            "DescribeVpcs",
            &[param("VpcId.1", &vpc_id)],
        )
        .await;
        assert_eq!(
            element(&described, "cidrBlock").as_deref(),
            Some(cidr_block.as_str()),
            "case {case:?}: expected the describe to report {cidr_block:?}, got {described}"
        );
        let held = allocations(case, &router, &pool_id).await;
        assert_eq!(
            held,
            vec![(
                cidr_block.clone(),
                ec2::state::IPAM_POOL_ALLOCATION_RESOURCE_TYPE_VPC.to_string(),
                Some(vpc_id.clone()),
            )],
            "case {case:?}: expected the pool to hold one allocation for the VPC"
        );

        // And the space is given back when the VPC is deleted, so the pool may hand it out
        // again rather than reporting it assigned to a VPC that is gone.
        ok(case, &router, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
        let after = allocations(case, &router, &pool_id).await;
        assert!(
            after.is_empty(),
            "case {case:?}: expected the deleted VPC to release its allocation, got {after:?}"
        );
    }
}

/// The pool's own default allocation netmask length is what a request that names neither a
/// block nor a size gets: "the default netmask length for allocations added to this pool"
/// (`IpamPool$AllocationDefaultNetmaskLength`).
#[tokio::test]
async fn a_pool_with_a_default_netmask_length_sizes_the_block_itself() {
    let case = "a pool whose default netmask length is /26";
    let (router, pool_id) =
        pool_with_space(case, &[param("AllocationDefaultNetmaskLength", "26")]).await;
    let created = ok(
        case,
        &router,
        "CreateVpc",
        &[param("Ipv4IpamPoolId", &pool_id)],
    )
    .await;
    let cidr_block = member(case, &created, "cidrBlock");
    let (_, _, prefix_length) = bounds(case, &cidr_block);
    assert_eq!(
        prefix_length, 26,
        "case {case:?}: expected the pool's default /26, got {cidr_block:?}"
    );

    // A request that names a size of its own overrides that default, which is the other half
    // of the same sentence.
    let case = "a request whose own size overrides the pool's default";
    let created = ok(
        case,
        &router,
        "CreateVpc",
        &[
            param("Ipv4IpamPoolId", &pool_id),
            param("Ipv4NetmaskLength", "24"),
        ],
    )
    .await;
    let second = member(case, &created, "cidrBlock");
    let (_, _, prefix_length) = bounds(case, &second);
    assert_eq!(
        prefix_length, 24,
        "case {case:?}: expected the /24 the request asked for, got {second:?}"
    );
    assert_ne!(
        second, cidr_block,
        "case {case:?}: expected a block the first VPC does not hold"
    );
}

/// Two VPCs out of one pool never overlap: the space one holds is space the pool reports as
/// assigned, which is what the next choice is made against.
#[tokio::test]
async fn two_vpcs_out_of_one_pool_hold_different_space() {
    let case = "four /18 VPCs out of one /16";
    let (router, pool_id) = pool_with_space(case, &[]).await;
    let mut blocks: Vec<(u32, u32)> = Vec::new();
    for index in 0..4 {
        let created = ok(
            case,
            &router,
            "CreateVpc",
            &[
                param("Ipv4IpamPoolId", &pool_id),
                param("Ipv4NetmaskLength", "18"),
            ],
        )
        .await;
        let cidr_block = member(case, &created, "cidrBlock");
        let (first, last, _) = bounds(case, &cidr_block);
        for (held_first, held_last) in &blocks {
            assert!(
                last < *held_first || first > *held_last,
                "case {case:?}: VPC {index} got {cidr_block:?} ({first}..={last}), which \
                 overlaps {held_first}..={held_last}"
            );
        }
        blocks.push((first, last));
    }
    assert_eq!(
        blocks.len(),
        4,
        "case {case:?}: expected four blocks, got {blocks:?}"
    );
    let held = allocations(case, &router, &pool_id).await;
    assert_eq!(
        held.len(),
        4,
        "case {case:?}: expected the pool to report all four allocations, got {held:?}"
    );
}

/// What a request the pool cannot serve is refused with — each with the code and, where EC2
/// words one, the sentence that says why, and with no VPC created.
#[tokio::test]
async fn a_request_the_pool_cannot_serve_is_refused() {
    let case = "the pool every row below names";
    let (router, pool_id) = pool_with_space(case, &[]).await;
    // One VPC already holding space, for the overlap row.
    let held = {
        let created = ok(
            case,
            &router,
            "CreateVpc",
            &[
                param("Ipv4IpamPoolId", &pool_id),
                param("CidrBlock", "10.200.64.0/18"),
            ],
        )
        .await;
        member(case, &created, "cidrBlock")
    };
    assert_eq!(
        held, "10.200.64.0/18",
        "the row below needs the block the create above took"
    );
    // A pool with a minimum allocation size, for the non-compliant-size row.
    let (bounded_router, bounded_pool) = pool_with_space(
        "a pool that allocates no block smaller than a /24",
        &[param("AllocationMinNetmaskLength", "24")],
    )
    .await;
    // A pool of the other family, for the family row.
    let other_family = {
        let ipam = ok(case, &router, "CreateIpam", &[]).await;
        let scope_id = member(case, &ipam, "privateDefaultScopeId");
        let created = ok(
            case,
            &router,
            "CreateIpamPool",
            &[
                param("IpamScopeId", &scope_id),
                param("AddressFamily", ec2::state::ADDRESS_FAMILY_IPV6),
            ],
        )
        .await;
        member(case, &created, "ipamPoolId")
    };

    // (case, the router, the members, the code)
    let rows: Vec<(&str, &MultiServiceRouter, Vec<Param>, &str)> = vec![
        (
            "a pool with neither a size nor a block",
            &router,
            vec![param("Ipv4IpamPoolId", &pool_id)],
            ec2::state::INVALID_PARAMETER_COMBINATION,
        ),
        (
            "a block and a size together",
            &router,
            vec![
                param("Ipv4IpamPoolId", &pool_id),
                param("CidrBlock", "10.200.1.0/24"),
                param("Ipv4NetmaskLength", "24"),
            ],
            ec2::state::INVALID_PARAMETER_COMBINATION,
        ),
        (
            "a size with no pool at all",
            &router,
            vec![
                param("CidrBlock", "10.201.0.0/16"),
                param("Ipv4NetmaskLength", "24"),
            ],
            ec2::state::INVALID_PARAMETER_COMBINATION,
        ),
        (
            "a pool the account does not hold",
            &router,
            vec![
                param("Ipv4IpamPoolId", "ipam-pool-0123456789abcdef0"),
                param("Ipv4NetmaskLength", "24"),
            ],
            ec2::state::INVALID_IPAM_POOL_ID_NOT_FOUND,
        ),
        (
            "a pool of the other address family",
            &router,
            vec![
                param("Ipv4IpamPoolId", &other_family),
                param("Ipv4NetmaskLength", "24"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
        (
            "a block outside the pool's space",
            &router,
            vec![
                param("Ipv4IpamPoolId", &pool_id),
                param("CidrBlock", "10.210.1.0/24"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
        (
            "a block meeting one a VPC already holds",
            &router,
            vec![
                param("Ipv4IpamPoolId", &pool_id),
                param("CidrBlock", "10.200.65.0/24"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
        (
            "a size no VPC block may have",
            &router,
            vec![
                param("Ipv4IpamPoolId", &pool_id),
                param(
                    "Ipv4NetmaskLength",
                    &(ec2::state::MAX_VPC_IPV4_PREFIX + 1).to_string(),
                ),
            ],
            ec2::state::INVALID_VPC_RANGE,
        ),
        (
            "a block larger than the pool's space",
            &router,
            vec![
                param("Ipv4IpamPoolId", &pool_id),
                param(
                    "Ipv4NetmaskLength",
                    &ec2::state::MIN_VPC_IPV4_PREFIX.to_string(),
                ),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
        (
            "a size the pool's own rules exclude",
            &bounded_router,
            vec![
                param("Ipv4IpamPoolId", &bounded_pool),
                param("Ipv4NetmaskLength", "20"),
            ],
            ec2::state::INVALID_PARAMETER_VALUE,
        ),
    ];

    for (case, router, params, code) in rows {
        let before = elements(
            &ok(case, router, "DescribeVpcs", &[]).await,
            "vpcId",
        );
        let (status, body) = call(router, "CreateVpc", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            code,
            "case {case:?}: expected the code {code:?}, got {body}"
        );
        let after = elements(
            &ok(case, router, "DescribeVpcs", &[]).await,
            "vpcId",
        );
        assert_eq!(
            after, before,
            "case {case:?}: the refusal must have created no VPC"
        );
    }
}

/// The SENTENCE a request that names a pool but neither a block nor a size is refused with:
/// the pool cannot size the block, so the request has to, and the message says so.
#[tokio::test]
async fn a_pool_with_no_default_size_says_exactly_one_of_the_two_is_needed() {
    let case = "a pool with no default allocation netmask length";
    let (router, pool_id) = pool_with_space(case, &[]).await;
    let (status, body) = call(
        &router,
        "CreateVpc",
        &[param("Ipv4IpamPoolId", &pool_id)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body).as_str(),
        ec2::state::INVALID_PARAMETER_COMBINATION,
        "case {case:?}: expected {} , got {body}",
        ec2::state::INVALID_PARAMETER_COMBINATION
    );
    assert_eq!(
        error_message(&body),
        "The request cannot contain empty CIDR and Netmask Length for a pool with no default \
         allocation netmask length set. Exactly one of them should be provided.",
        "case {case:?}: expected the sentence EC2 words for this, got {body}"
    );
}

/// A VPC is a resource OF A REGION, so a pool that is not available for allocations in the
/// Region the VPC is created in serves no VPC at all: the resource type is not valid for that
/// pool, and the refusal names the pool's locale as every read of the pool reports it.
///
/// The rows differ along WHY the pool does not allocate here: a pool created with no locale,
/// which "means any locale" and is therefore available in none, and a pool whose locale is
/// another Region the same IPAM operates in. Both carry space and admit the size asked for, so
/// the locale is the only thing between the request and a block.
#[tokio::test]
async fn a_pool_that_does_not_allocate_in_this_region_serves_no_vpc() {
    // (case, the Locale the pool is created with, the locale the refusal must name)
    let rows: [(&str, Option<&str>, &str); 2] = [
        (
            "a top-level pool created with no locale",
            None,
            ec2::state::IPAM_POOL_LOCALE_NONE,
        ),
        (
            "a pool whose locale is another Region of the same IPAM",
            Some(OTHER_REGION),
            OTHER_REGION,
        ),
    ];

    for (case, locale, reported_locale) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        // The IPAM operates in both Regions, so the locale of the second row is one a pool may
        // name at all — what refuses the VPC is that it is not THIS Region.
        let ipam = ok(
            case,
            &router,
            "CreateIpam",
            &[param("OperatingRegion.1.RegionName", OTHER_REGION)],
        )
        .await;
        let scope_id = member(case, &ipam, "privateDefaultScopeId");
        let mut params = vec![
            param("IpamScopeId", &scope_id),
            param("AddressFamily", ec2::state::ADDRESS_FAMILY_IPV4),
        ];
        if let Some(locale) = locale {
            params.push(param("Locale", locale));
        }
        let created = ok(case, &router, "CreateIpamPool", &params).await;
        let pool_id = member(case, &created, "ipamPoolId");
        ok(
            case,
            &router,
            "ProvisionIpamPoolCidr",
            &[param("IpamPoolId", &pool_id), param("Cidr", POOL_SPACE)],
        )
        .await;

        // Each form of the request — a size for the pool to choose, and a block the request
        // names itself — is refused the same way: the pool allocates nothing here.
        for (form, members) in [
            ("a size the pool chooses from", vec![param("Ipv4NetmaskLength", "24")]),
            ("a block the request names", vec![param("CidrBlock", "10.200.9.0/24")]),
        ] {
            let mut request = vec![param("Ipv4IpamPoolId", &pool_id)];
            request.extend(members);
            let (status, body) = call(&router, "CreateVpc", &request).await;
            assert_eq!(
                status, 400,
                "case {case:?} with {form}: expected HTTP 400, got {status} with body {body}"
            );
            assert_eq!(
                error_code(&body).as_str(),
                ec2::state::INVALID_PARAMETER_VALUE,
                "case {case:?} with {form}: expected the parameter-value class, got {body}"
            );
            assert_eq!(
                error_message(&body),
                format!(
                    "The resource type vpc is not valid for pool {pool_id} with \
                     {reported_locale} locale."
                ),
                "case {case:?} with {form}: expected the sentence EC2 words for a pool that \
                 does not allocate here, got {body}"
            );
        }
        let vpcs = elements(&ok(case, &router, "DescribeVpcs", &[]).await, "cidrBlock");
        assert!(
            !vpcs.iter().any(|cidr| cidr.starts_with("10.200.")),
            "case {case:?}: the refusals must have created no VPC out of the pool, got {vpcs:?}"
        );
    }
}

/// A size outside the pool's own allocation netmask rules is refused naming the size asked for
/// and the bound the pool allows: "the minimum netmask length required for CIDR allocations in
/// this IPAM pool to be compliant" and "the maximum netmask length possible"
/// (`IpamPool$AllocationMinNetmaskLength`, `$AllocationMaxNetmaskLength`) are the two bounds a
/// VPC's block is held to as well.
///
/// The rows differ along which bound the request breaks and how it asks for the size: each
/// published bound, one past it on either side, and a block the request names itself rather
/// than a size for the pool to choose.
#[tokio::test]
async fn a_size_outside_the_pools_allocation_rules_names_the_bound_it_broke() {
    // The pool every row below allocates out of: blocks between a /20 and a /26.
    let case = "a pool that allocates /20 to /26 blocks";
    let (router, pool_id) = pool_with_space(
        case,
        &[
            param("AllocationMinNetmaskLength", "20"),
            param("AllocationMaxNetmaskLength", "26"),
        ],
    )
    .await;

    // (case, the members beside Ipv4IpamPoolId, the message — None for a request the pool
    //  serves)
    let rows: Vec<(&str, Vec<Param>, Option<String>)> = vec![
        (
            "the largest size the pool allows",
            vec![param("Ipv4NetmaskLength", "20")],
            None,
        ),
        (
            "the smallest size the pool allows",
            vec![param("Ipv4NetmaskLength", "26")],
            None,
        ),
        (
            "a size inside both bounds",
            vec![param("Ipv4NetmaskLength", "24")],
            None,
        ),
        (
            "one bit larger than the largest size the pool allows",
            vec![param("Ipv4NetmaskLength", "19")],
            Some(format!(
                "The requested netmask length /19 is larger than allowed by pool {pool_id}. \
                 Largest netmask length allowed is /20"
            )),
        ),
        (
            "one bit smaller than the smallest size the pool allows",
            vec![param("Ipv4NetmaskLength", "27")],
            Some(format!(
                "The requested netmask length /27 is smaller than allowed by pool {pool_id}. \
                 Smallest netmask length allowed is /26"
            )),
        ),
        (
            "a block the request names whose own size is too large",
            vec![param("CidrBlock", "10.200.0.0/18")],
            Some(format!(
                "The requested netmask length /18 is larger than allowed by pool {pool_id}. \
                 Largest netmask length allowed is /20"
            )),
        ),
        (
            "a block the request names whose own size is too small",
            vec![param("CidrBlock", "10.200.0.0/28")],
            Some(format!(
                "The requested netmask length /28 is smaller than allowed by pool {pool_id}. \
                 Smallest netmask length allowed is /26"
            )),
        ),
    ];

    for (case, members, refusal) in rows {
        let mut request = vec![param("Ipv4IpamPoolId", &pool_id)];
        request.extend(members);
        let (status, body) = call(&router, "CreateVpc", &request).await;
        match refusal {
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the pool to serve this size, got {status} with body \
                 {body}"
            ),
            Some(message) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with body {body}"
                );
                assert_eq!(
                    error_code(&body).as_str(),
                    ec2::state::INVALID_PARAMETER_VALUE,
                    "case {case:?}: expected the parameter-value class, got {body}"
                );
                assert_eq!(
                    error_message(&body), message,
                    "case {case:?}: expected the sentence naming the bound, got {body}"
                );
            }
        }
    }
}

/// A pool that requires resource tags allocates only to a resource carrying them: "resources
/// that do not have these tags will not be allowed to allocate space from the pool"
/// (`IpamPool$AllocationResourceTags`).
///
/// The rows differ along how the VPC's own tags relate to the required set: none at all, the
/// required key with another value, only one of the two required pairs, and both of them
/// beside a tag the pool never asked for.
#[tokio::test]
async fn a_vpc_without_the_tags_the_pool_requires_is_refused() {
    let case = "a pool that requires Owner=TeamA and Tier=web";
    let (router, pool_id) = pool_with_space(
        case,
        &[
            param("AllocationResourceTag.1.Key", "Owner"),
            param("AllocationResourceTag.1.Value", "TeamA"),
            param("AllocationResourceTag.2.Key", "Tier"),
            param("AllocationResourceTag.2.Value", "web"),
        ],
    )
    .await;

    /// One tag on the VPC the row creates.
    fn tag(index: usize, key: &str, value: &str) -> Vec<Param> {
        vec![
            param("TagSpecification.1.ResourceType", "vpc"),
            param(&format!("TagSpecification.1.Tag.{index}.Key"), key),
            param(&format!("TagSpecification.1.Tag.{index}.Value"), value),
        ]
    }

    // (case, the tags the create carries, whether the pool allocates to it)
    let rows: Vec<(&str, Vec<Param>, bool)> = vec![
        ("no tags at all", Vec::new(), false),
        (
            "the required key with another value",
            tag(1, "Owner", "TeamB"),
            false,
        ),
        ("only one of the required pairs", tag(1, "Owner", "TeamA"), false),
        (
            "both required pairs beside one the pool never asked for",
            {
                let mut tags = tag(1, "Owner", "TeamA");
                tags.extend(tag(2, "Tier", "web"));
                tags.extend(tag(3, "Name", "anything"));
                tags
            },
            true,
        ),
    ];

    for (case, tags, served) in rows {
        let mut request = vec![
            param("Ipv4IpamPoolId", &pool_id),
            param("Ipv4NetmaskLength", "24"),
        ];
        request.extend(tags);
        let (status, body) = call(&router, "CreateVpc", &request).await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the tagged VPC to be served, got {status} with body \
                 {body}"
            );
            let vpc_id = member(case, &body, "vpcId");
            let cidr_block = member(case, &body, "cidrBlock");
            let held = allocations(case, &router, &pool_id).await;
            assert_eq!(
                held,
                vec![(
                    cidr_block,
                    ec2::state::IPAM_POOL_ALLOCATION_RESOURCE_TYPE_VPC.to_string(),
                    Some(vpc_id),
                )],
                "case {case:?}: expected the pool to hold the allocation it served"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body).as_str(),
            ec2::state::INVALID_PARAMETER_VALUE,
            "case {case:?}: expected the parameter-value class, got {body}"
        );
        let held = allocations(case, &router, &pool_id).await;
        assert!(
            held.is_empty(),
            "case {case:?}: the refusal must have taken no space out of the pool, got {held:?}"
        );
    }
}
