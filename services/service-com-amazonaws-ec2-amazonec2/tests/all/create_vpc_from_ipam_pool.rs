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
async fn pool_with_space(case: &str, extra: &[Param]) -> (MultiServiceRouter, String) {
    let (router, _engine) = super::ec2_query::emulator();
    let ipam = ok(case, &router, "CreateIpam", &[]).await;
    let scope_id = member(case, &ipam, "privateDefaultScopeId");
    let mut params = vec![
        param("IpamScopeId", &scope_id),
        param("AddressFamily", ec2::state::ADDRESS_FAMILY_IPV4),
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
