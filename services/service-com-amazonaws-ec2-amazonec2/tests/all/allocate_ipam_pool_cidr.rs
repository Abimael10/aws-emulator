// SPDX-License-Identifier: CC0-1.0

//! What `AllocateIpamPoolCidr` takes out of an IPAM pool, and what it refuses — driven over
//! `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! One rule: an allocation assigns ONE range out of the space provisioned to the pool the
//! request names, minus everything already assigned out of it, and the store then holds it —
//! so "the returned CIDR will not overlap with any other allocations from the same pool" and
//! every later reader of the store reports the same range. The members the service owns are
//! derived rather than taken from the request: the `IpamPoolAllocationId`, the `custom`
//! resource type an allocation nothing else holds carries, the Region the pool allocates in
//! and the account that owns it. A request that names a SIZE rather than a range asks the
//! service to choose, which it does out of the pool's free space in the order the store holds
//! it, steered by the ranges the request allows and disallows; a request that names neither
//! falls back to the pool's own default netmask length. A range the pool could never hand out
//! — one of the other family, one outside the pool's space, one meeting a range already
//! assigned, one of a size the pool's rules exclude — is refused before anything is stored.
//!
//! The rows of each table differ along what that rule generalises over: the address family,
//! the size of the range, how the request spells it, whether it names a range or a size,
//! whether the pool is top-level or a pool was created within it, and how the requested range
//! relates to the space already assigned.
//!
//! The allocations are read back through `GetIpamPoolAllocations`, which this change
//! implements as the round trip of this write (its own stub issue is
//! `20261103T071133Z-ec2-getipampoolallocations-stub.md`).
//!
//! The suite keeps its own `call`/`ok`/`member` wrappers over the shared [`super::ec2_query`]
//! harness, which is how every other suite in this crate is arranged.

use ec2::state::{
    ADDRESS_FAMILY_IPV4, ADDRESS_FAMILY_IPV6, DEPENDENCY_VIOLATION, DRY_RUN_OPERATION,
    DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS, IDEMPOTENT_PARAMETER_MISMATCH,
    IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE, INVALID_IPAM_POOL_ID_MALFORMED,
    INVALID_IPAM_POOL_ID_NOT_FOUND, INVALID_PARAMETER_COMBINATION, INVALID_PARAMETER_VALUE,
    IPAM_POOL_ALLOCATION_ID_PREFIX, IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM,
    IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL, MISSING_PARAMETER,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, emulator, error_code, error_message};
use super::wire::{Param, child_text, document, is_long_id, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The allocation this suite drives.
const ALLOCATE: &str = "AllocateIpamPoolCidr";

/// The read every allocation of this suite is proved through.
const GET_ALLOCATIONS: &str = "GetIpamPoolAllocations";

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

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// Assert that `action` was refused HTTP 400 with `code` and exactly `message`.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    code: &str,
    message: &str,
) {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} from {action}, got {status} with body \
         {body}"
    );
    let actual_code = error_code(&body);
    assert_eq!(
        actual_code, code,
        "case {case:?}: expected the code {code:?} from {action}, got {actual_code:?} in {body}"
    );
    let actual_message = error_message(&body);
    assert_eq!(
        actual_message, message,
        "case {case:?}: expected the message {message:?}, got {actual_message:?}"
    );
}

/// Create one IPAM and answer its default private scope id, the scope every pool of this suite
/// is created in.
async fn create_scope(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "CreateIpam", &[]).await;
    member(case, &body, "privateDefaultScopeId")
}

/// Create one IPAM pool of `family` in `scope_id`, with `extra` beside it, and answer the
/// `IpamPoolId` the service minted.
async fn create_pool(
    case: &str,
    router: &MultiServiceRouter,
    scope_id: &str,
    family: &str,
    extra: &[Param],
) -> String {
    let mut params = vec![
        param("IpamScopeId", scope_id),
        param("AddressFamily", family),
    ];
    params.extend(extra.iter().cloned());
    let body = ok(case, router, "CreateIpamPool", &params).await;
    member(case, &body, "ipamPoolId")
}

/// Provision `cidr` to the pool `pool_id` names, which is what gives it space to allocate out
/// of.
async fn provision(case: &str, router: &MultiServiceRouter, pool_id: &str, cidr: &str) {
    ok(
        case,
        router,
        "ProvisionIpamPoolCidr",
        &[param("IpamPoolId", pool_id), param("Cidr", cidr)],
    )
    .await;
}

/// One pool with `cidr` provisioned to it: the router, the scope and the pool id.
async fn pool_with_space(
    case: &str,
    family: &str,
    cidr: &str,
) -> (MultiServiceRouter, String, String) {
    let (router, _engine) = emulator();
    let scope_id = create_scope(case, &router).await;
    let pool_id = create_pool(case, &router, &scope_id, family, &[]).await;
    provision(case, &router, &pool_id, cidr).await;
    (router, scope_id, pool_id)
}

/// One allocation as a reader of the store reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAllocation {
    /// The `ipamPoolAllocationId` the service minted for it.
    id: String,
    /// The range it assigns, as the store spells it.
    cidr: String,
    /// The `IpamPoolAllocationResourceType` member it carries.
    resource_type: String,
    /// The `resourceId` it names, when it names one.
    resource_id: Option<String>,
    /// The description the caller gave it, when it gave one.
    description: Option<String>,
}

/// Every allocation `body` carries, in the order it reported them.
fn reported_all(case: &str, body: &str) -> Vec<ReportedAllocation> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolAllocationId").is_some())
        .map(|node| ReportedAllocation {
            id: child_text(node, "ipamPoolAllocationId").unwrap_or_default(),
            cidr: child_text(node, "cidr").unwrap_or_default(),
            resource_type: child_text(node, "resourceType").unwrap_or_default(),
            resource_id: child_text(node, "resourceId"),
            description: child_text(node, "description"),
        })
        .collect()
}

/// The one allocation `body` carries, or a panic naming the case.
fn reported_one(case: &str, body: &str) -> ReportedAllocation {
    let mut all = reported_all(case, body);
    assert_eq!(
        all.len(),
        1,
        "case {case:?}: expected exactly one reported allocation, got {all:?} in {body}"
    );
    all.remove(0)
}

/// Every allocation `GetIpamPoolAllocations` reports for the pool, sorted by range.
async fn held(case: &str, router: &MultiServiceRouter, pool_id: &str) -> Vec<ReportedAllocation> {
    let body = ok(
        case,
        router,
        GET_ALLOCATIONS,
        &[param("IpamPoolId", pool_id)],
    )
    .await;
    let mut reported = reported_all(case, &body);
    reported.sort_by(|left, right| left.cidr.cmp(&right.cidr));
    reported
}

// ---------------------------------------------------------------------------
// What an allocation stores, and what a read of the pool then reports
// ---------------------------------------------------------------------------

/// What one row of the allocation table asks for.
#[derive(Debug, Clone, Copy)]
enum Asked {
    /// The request names the range itself (`$Cidr`).
    Cidr(&'static str),
    /// The request names the SIZE and leaves the choice to the service (`$NetmaskLength`).
    NetmaskLength(u32),
    /// The request names neither, so the pool's own default netmask length decides.
    Nothing,
}

/// One row of the allocation table.
struct Allocation {
    /// What makes this row different from the ones beside it.
    case: &'static str,
    /// The `AddressFamily` the pool holds.
    family: &'static str,
    /// The range provisioned to the pool, which is the space the allocation comes out of.
    space: &'static str,
    /// The members the pool itself is created with beyond its scope and family.
    pool: &'static [(&'static str, &'static str)],
    /// What the request asks for.
    asked: Asked,
    /// The range the allocation must hold.
    expected: &'static str,
}

/// An allocation holds the range the request asked for — or the one the service chose out of the
/// pool's free space — spelled as the store spells a range, and a read of the pool reports
/// exactly that allocation.
#[tokio::test]
async fn an_allocation_holds_one_range_of_the_pools_space_and_reads_back() {
    // The rows differ along the family, the size of the range, the spelling of the request and
    // whether the request names a range, a size, or neither.
    let rows: &[Allocation] = &[
        Allocation {
            case: "an IPv4 range the request names itself",
            family: ADDRESS_FAMILY_IPV4,
            space: "10.0.0.0/16",
            pool: &[],
            asked: Asked::Cidr("10.0.1.0/24"),
            expected: "10.0.1.0/24",
        },
        Allocation {
            case: "an IPv4 range spelled with its host bits set",
            family: ADDRESS_FAMILY_IPV4,
            space: "10.0.0.0/16",
            pool: &[],
            asked: Asked::Cidr("10.0.2.37/24"),
            expected: "10.0.2.0/24",
        },
        Allocation {
            case: "a single IPv4 address, which is a /32 range",
            family: ADDRESS_FAMILY_IPV4,
            space: "192.168.4.0/24",
            pool: &[],
            asked: Asked::Cidr("192.168.4.9/32"),
            expected: "192.168.4.9/32",
        },
        Allocation {
            case: "an IPv4 size, which the service chooses the range for",
            family: ADDRESS_FAMILY_IPV4,
            space: "10.0.0.0/16",
            pool: &[],
            asked: Asked::NetmaskLength(24),
            expected: "10.0.0.0/24",
        },
        Allocation {
            case: "an IPv4 size as long as the space itself",
            family: ADDRESS_FAMILY_IPV4,
            space: "172.16.8.0/22",
            pool: &[],
            asked: Asked::NetmaskLength(22),
            expected: "172.16.8.0/22",
        },
        Allocation {
            case: "no size at all, which the pool's default netmask length decides",
            family: ADDRESS_FAMILY_IPV4,
            space: "10.0.0.0/16",
            pool: &[("AllocationDefaultNetmaskLength", "26")],
            asked: Asked::Nothing,
            expected: "10.0.0.0/26",
        },
        Allocation {
            case: "an IPv6 range the request names itself",
            family: ADDRESS_FAMILY_IPV6,
            space: "2001:db8::/56",
            pool: &[],
            asked: Asked::Cidr("2001:db8:0:1::/64"),
            expected: "2001:db8:0:1::/64",
        },
        Allocation {
            case: "an IPv6 size, which the service chooses the range for",
            family: ADDRESS_FAMILY_IPV6,
            space: "2001:db8::/56",
            pool: &[],
            asked: Asked::NetmaskLength(64),
            expected: "2001:db8::/64",
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, _engine) = emulator();
        let scope_id = create_scope(case, &router).await;
        let pool: Vec<Param> = row
            .pool
            .iter()
            .map(|(name, value)| param(name, value))
            .collect();
        let pool_id = create_pool(case, &router, &scope_id, row.family, &pool).await;
        provision(case, &router, &pool_id, row.space).await;

        let mut params = vec![
            param("IpamPoolId", &pool_id),
            param("Description", "space for the service tier"),
            param("TagSpecification.1.ResourceType", "ipam-pool-allocation"),
            param("TagSpecification.1.Tag.1.Key", "Name"),
            param("TagSpecification.1.Tag.1.Value", "service-tier"),
        ];
        match row.asked {
            Asked::Cidr(cidr) => params.push(param("Cidr", cidr)),
            Asked::NetmaskLength(length) => {
                params.push(param("NetmaskLength", &length.to_string()));
            }
            Asked::Nothing => {}
        }

        let body = ok(case, &router, ALLOCATE, &params).await;
        let answered = reported_one(case, &body);
        assert_eq!(
            answered.cidr, row.expected,
            "case {case:?}: expected the allocation to hold {expected:?}, got {answered:?} in \
             {body}",
            expected = row.expected,
        );
        assert!(
            is_long_id(IPAM_POOL_ALLOCATION_ID_PREFIX, &answered.id),
            "case {case:?}: expected an {IPAM_POOL_ALLOCATION_ID_PREFIX}- id, got {answered:?}"
        );
        assert_eq!(
            answered.resource_type, IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM,
            "case {case:?}: expected the resource type \
             {IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM:?}, got {answered:?}"
        );
        assert_eq!(
            answered.resource_id, None,
            "case {case:?}: an allocation no resource holds names none, got {answered:?}"
        );
        assert_eq!(
            answered.description.as_deref(),
            Some("space for the service tier"),
            "case {case:?}: expected the description the request gave, got {answered:?}"
        );
        assert_eq!(
            element(&body, "resourceOwner").as_deref(),
            Some(engine::emulator_account_id()),
            "case {case:?}: expected the allocation to be owned by the account, got {body}"
        );
        assert_eq!(
            element(&body, "value").as_deref(),
            Some("service-tier"),
            "case {case:?}: expected the tag the request gave, got {body}"
        );

        // The round trip: the pool reports exactly the allocation the call answered.
        let reported = held(case, &router, &pool_id).await;
        assert_eq!(
            reported,
            vec![answered.clone()],
            "case {case:?}: expected a read of the pool to report the allocation the call \
             answered, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The space one allocation takes is space the next one cannot
// ---------------------------------------------------------------------------

/// Successive allocations of one size walk the pool's free space rather than handing the same
/// range out twice, and a range the request names that meets one of them is refused.
#[tokio::test]
async fn allocations_out_of_one_pool_never_overlap() {
    let case = "three /24 allocations out of one /16";
    let (router, _scope_id, pool_id) = pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16")
        .await;

    let mut answered: Vec<String> = Vec::new();
    for _ in 0..3 {
        let body = ok(
            case,
            &router,
            ALLOCATE,
            &[param("IpamPoolId", &pool_id), param("NetmaskLength", "24")],
        )
        .await;
        answered.push(reported_one(case, &body).cidr);
    }
    assert_eq!(
        answered,
        vec![
            "10.0.0.0/24".to_string(),
            "10.0.1.0/24".to_string(),
            "10.0.2.0/24".to_string()
        ],
        "case {case:?}: expected each allocation to take the first free block, got {answered:?}"
    );

    // A range the request names that meets one already assigned is refused, and a range that
    // meets none of them is given.
    for (overlap_case, requested, held_range) in [
        ("exactly a range already allocated", "10.0.1.0/24", "10.0.1.0/24"),
        ("a range inside one already allocated", "10.0.2.128/25", "10.0.2.0/24"),
        ("a range containing one already allocated", "10.0.0.0/20", "10.0.0.0/24"),
    ] {
        refused(
            overlap_case,
            &router,
            ALLOCATE,
            &[param("IpamPoolId", &pool_id), param("Cidr", requested)],
            INVALID_PARAMETER_VALUE,
            &format!(
                "Invalid value '{requested}' for cidr. It overlaps with the CIDR \
                 '{held_range}' already allocated from the IPAM pool '{pool_id}'"
            ),
        )
        .await;
    }
    let free = ok(
        case,
        &router,
        ALLOCATE,
        &[param("IpamPoolId", &pool_id), param("Cidr", "10.0.9.0/24")],
    )
    .await;
    assert_eq!(
        reported_one(case, &free).cidr,
        "10.0.9.0/24",
        "case {case:?}: a range meeting nothing already assigned must be given"
    );

    let reported: Vec<String> = held(case, &router, &pool_id)
        .await
        .into_iter()
        .map(|allocation| allocation.cidr)
        .collect();
    assert_eq!(
        reported,
        vec![
            "10.0.0.0/24".to_string(),
            "10.0.1.0/24".to_string(),
            "10.0.2.0/24".to_string(),
            "10.0.9.0/24".to_string()
        ],
        "case {case:?}: expected the pool to report every allocation it holds, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// The ranges a request allows and disallows steer the choice
// ---------------------------------------------------------------------------

/// `AllowedCidrs` and `DisallowedCidrs` steer which block a request naming a SIZE is given,
/// over rows that differ along which of the two lists is used, how many entries it carries and
/// where in the space they fall.
#[tokio::test]
async fn allowed_and_disallowed_cidrs_steer_the_chosen_block() {
    // (case, the allowed ranges, the disallowed ranges, the block the pool must choose)
    let rows: &[(&str, &[&str], &[&str], &str)] = &[
        ("neither list, so the first block of the space", &[], &[], "10.0.0.0/24"),
        (
            "one disallowed range over the first block",
            &[],
            &["10.0.0.0/24"],
            "10.0.1.0/24",
        ),
        (
            "two disallowed ranges, one of them larger than a block",
            &[],
            &["10.0.0.0/23", "10.0.2.0/24"],
            "10.0.3.0/24",
        ),
        (
            "one allowed range further into the space",
            &["10.0.8.0/22"],
            &[],
            "10.0.8.0/24",
        ),
        (
            "two allowed ranges, the first of them excluded as well",
            &["10.0.8.0/24", "10.0.12.0/22"],
            &["10.0.8.0/24"],
            "10.0.12.0/24",
        ),
    ];

    for (case, allowed, disallowed, expected) in rows {
        let (router, _scope_id, pool_id) =
            pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;
        let mut params = vec![param("IpamPoolId", &pool_id), param("NetmaskLength", "24")];
        for (index, range) in allowed.iter().enumerate() {
            params.push(param(&format!("AllowedCidr.{}", index + 1), range));
        }
        for (index, range) in disallowed.iter().enumerate() {
            params.push(param(&format!("DisallowedCidr.{}", index + 1), range));
        }
        let body = ok(case, &router, ALLOCATE, &params).await;
        let answered = reported_one(case, &body);
        assert_eq!(
            &answered.cidr, expected,
            "case {case:?}: expected the pool to choose {expected:?}, got {answered:?}"
        );
    }

    // A space where every allowed range is excluded holds no block the pool may return.
    let case = "an allowed range that is entirely disallowed";
    let (router, _scope_id, pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;
    refused(
        case,
        &router,
        ALLOCATE,
        &[
            param("IpamPoolId", &pool_id),
            param("NetmaskLength", "24"),
            param("AllowedCidr.1", "10.0.5.0/24"),
            param("DisallowedCidr.1", "10.0.5.0/24"),
        ],
        INVALID_PARAMETER_VALUE,
        &format!(
            "Invalid value '24' for netmaskLength. The IPAM pool '{pool_id}' has no free CIDR \
             of that size"
        ),
    )
    .await;
}

// ---------------------------------------------------------------------------
// A preview assigns nothing
// ---------------------------------------------------------------------------

/// `PreviewNextCidr` answers the range the pool WOULD hand out and stores nothing, so two
/// previews answer the same range and the allocation that follows is given it.
#[tokio::test]
async fn a_preview_of_the_next_cidr_assigns_nothing() {
    let case = "a preview of the next /24 of a /16 pool";
    let (router, _scope_id, pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;

    for look in 1..=2 {
        let body = ok(
            case,
            &router,
            ALLOCATE,
            &[
                param("IpamPoolId", &pool_id),
                param("NetmaskLength", "24"),
                param("PreviewNextCidr", "true"),
            ],
        )
        .await;
        assert_eq!(
            element(&body, "cidr").as_deref(),
            Some("10.0.0.0/24"),
            "case {case:?}: look {look} must preview the first free block, got {body}"
        );
        assert_eq!(
            element(&body, "ipamPoolAllocationId"),
            None,
            "case {case:?}: look {look} previewed a range, so nothing was allocated an id: \
             {body}"
        );
        let reported = held(case, &router, &pool_id).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: look {look} must leave the pool with no allocation, got {reported:?}"
        );
    }

    let body = ok(
        case,
        &router,
        ALLOCATE,
        &[param("IpamPoolId", &pool_id), param("NetmaskLength", "24")],
    )
    .await;
    assert_eq!(
        reported_one(case, &body).cidr,
        "10.0.0.0/24",
        "case {case:?}: the allocation after a preview must be given the previewed range"
    );
}

// ---------------------------------------------------------------------------
// One token, one allocation
// ---------------------------------------------------------------------------

/// A replayed `ClientToken` answers the allocation it minted and hands out no second range;
/// replayed against another pool, or with a different request, it is refused.
#[tokio::test]
async fn a_replayed_client_token_answers_the_same_allocation() {
    let case = "one token replayed";
    let (router, scope_id, pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;
    let token = "allocation-token-1";
    let params = vec![
        param("IpamPoolId", &pool_id),
        param("NetmaskLength", "24"),
        param("ClientToken", token),
        param("Description", "the service tier"),
    ];

    let first = reported_one(case, &ok(case, &router, ALLOCATE, &params).await);
    let replay = reported_one(case, &ok(case, &router, ALLOCATE, &params).await);
    assert_eq!(
        replay, first,
        "case {case:?}: a replay must answer the allocation the token minted, got {replay:?}"
    );
    let reported = held(case, &router, &pool_id).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: a replay must hand out no second range, got {reported:?}"
    );

    // Each row changes ONE member of the request the token was first given with; an empty
    // value REMOVES that member from the replay.
    let rows: &[(&str, &[(&str, &str)])] = &[
        ("a different size", &[("NetmaskLength", "25")]),
        ("a different description", &[("Description", "something else")]),
        ("no description at all", &[("Description", "")]),
        (
            "a range instead of a size",
            &[("Cidr", "10.0.7.0/24"), ("NetmaskLength", "")],
        ),
        (
            "a tag the first request did not carry",
            &[
                ("TagSpecification.1.ResourceType", "ipam-pool-allocation"),
                ("TagSpecification.1.Tag.1.Key", "Name"),
                ("TagSpecification.1.Tag.1.Value", "other"),
            ],
        ),
    ];
    for (mismatch_case, changes) in rows {
        let mut replayed: Vec<Param> = params.clone();
        for (name, value) in *changes {
            replayed.retain(|(held_name, _)| held_name != name);
            if !value.is_empty() {
                replayed.push(param(name, value));
            }
        }
        refused(
            mismatch_case,
            &router,
            ALLOCATE,
            &replayed,
            IDEMPOTENT_PARAMETER_MISMATCH,
            IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE,
        )
        .await;
    }

    // The POOL is part of the request the token was given with: replayed against another pool,
    // the token names an allocation of a pool the caller did not name.
    let other_case = "the same token against another pool";
    let other_pool_id = create_pool(other_case, &router, &scope_id, ADDRESS_FAMILY_IPV4, &[]).await;
    provision(other_case, &router, &other_pool_id, "10.1.0.0/16").await;
    let mut replayed: Vec<Param> = params.clone();
    replayed.retain(|(name, _)| name != "IpamPoolId");
    replayed.push(param("IpamPoolId", &other_pool_id));
    refused(
        other_case,
        &router,
        ALLOCATE,
        &replayed,
        IDEMPOTENT_PARAMETER_MISMATCH,
        IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE,
    )
    .await;
    let reported = held(other_case, &router, &other_pool_id).await;
    assert!(
        reported.is_empty(),
        "case {other_case:?}: a refused replay must allocate nothing out of the other pool, got \
         {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// A pool within a pool holds an allocation of the pool it draws on
// ---------------------------------------------------------------------------

/// A range provisioned to a pool created WITHIN another is an allocation of that other pool —
/// "a CIDR assignment from an IPAM pool to another IPAM pool" — so it is reported as one, it is
/// space the source pool will not hand out again, and it goes back when the range leaves the
/// pool it was assigned to.
#[tokio::test]
async fn a_range_given_to_a_pool_within_another_is_an_allocation_of_it() {
    let case = "a /24 provisioned to a pool within a /16 pool";
    let (router, scope_id, source_pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;
    let child_pool_id = create_pool(
        case,
        &router,
        &scope_id,
        ADDRESS_FAMILY_IPV4,
        &[param("SourceIpamPoolId", &source_pool_id)],
    )
    .await;
    provision(case, &router, &child_pool_id, "10.0.0.0/24").await;

    let reported = held(case, &router, &source_pool_id).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?}: expected the source pool to report one allocation, got {reported:?}"
    );
    assert_eq!(
        reported[0].cidr, "10.0.0.0/24",
        "case {case:?}: expected the allocation to hold the provisioned range, got {reported:?}"
    );
    assert_eq!(
        reported[0].resource_type, IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL,
        "case {case:?}: expected the resource type \
         {IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL:?}, got {reported:?}"
    );
    assert_eq!(
        reported[0].resource_id.as_deref(),
        Some(child_pool_id.as_str()),
        "case {case:?}: expected the allocation to name the pool it was assigned to, got \
         {reported:?}"
    );

    // The source pool will not hand that space out again.
    let body = ok(
        case,
        &router,
        ALLOCATE,
        &[
            param("IpamPoolId", &source_pool_id),
            param("NetmaskLength", "24"),
        ],
    )
    .await;
    assert_eq!(
        reported_one(case, &body).cidr,
        "10.0.1.0/24",
        "case {case:?}: an allocation must skip the space the pool within it holds"
    );

    // A range that LEAVES the pool it was assigned to is the source pool's space again.
    ok(
        case,
        &router,
        "DeprovisionIpamPoolCidr",
        &[
            param("IpamPoolId", &child_pool_id),
            param("Cidr", "10.0.0.0/24"),
        ],
    )
    .await;
    let after: Vec<String> = held(case, &router, &source_pool_id)
        .await
        .into_iter()
        .map(|allocation| allocation.cidr)
        .collect();
    assert_eq!(
        after,
        vec!["10.0.1.0/24".to_string()],
        "case {case:?}: expected only the caller's own allocation to be left, got {after:?}"
    );
}

// ---------------------------------------------------------------------------
// An allocation keeps its pool from being deleted
// ---------------------------------------------------------------------------

/// "You cannot delete an IPAM pool if there are allocations in it or CIDRs provisioned to it":
/// a pool holding one is refused, and the refusal names the dependency a caller clears — the
/// provisioned range the allocation was carved out of, ahead of the allocation itself — while
/// `Cascade` is the one way to delete the pool along with both.
#[tokio::test]
async fn an_allocation_blocks_the_delete_of_its_pool() {
    let case = "a pool holding one allocation";
    let (router, _scope_id, pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;
    ok(
        case,
        &router,
        ALLOCATE,
        &[param("IpamPoolId", &pool_id), param("NetmaskLength", "24")],
    )
    .await;

    refused(
        case,
        &router,
        "DeleteIpamPool",
        &[param("IpamPoolId", &pool_id)],
        DEPENDENCY_VIOLATION,
        &format!(
            "The specified action is not valid for {pool_id}. There are one or more \
             pool cidrs provisioned in the pool."
        ),
    )
    .await;

    let body = ok(
        case,
        &router,
        "DeleteIpamPool",
        &[param("IpamPoolId", &pool_id), param("Cascade", "true")],
    )
    .await;
    assert!(
        element(&body, "state").is_some(),
        "case {case:?}: expected a cascading delete to report the pool's state, got {body}"
    );
}

// ---------------------------------------------------------------------------
// What an allocation refuses, and stores nothing for
// ---------------------------------------------------------------------------

/// One row of the refusal table.
struct Refusal {
    /// What makes this row different from the ones beside it.
    case: &'static str,
    /// The pool's own members beyond its scope and family, so a row can set allocation rules.
    pool: &'static [(&'static str, &'static str)],
    /// The request members beyond the pool id; an entry naming `IpamPoolId` replaces it.
    params: &'static [(&'static str, &'static str)],
    /// The error code EC2 answers.
    code: &'static str,
    /// The message EC2 answers, with `{pool}` standing for the pool's own id.
    message: &'static str,
}

/// Every request an allocation cannot serve is refused with EC2's own code and sentence, and
/// nothing is stored for it.
#[tokio::test]
async fn an_allocation_refuses_what_the_pool_cannot_hand_out() {
    // A well-formed `IpamPoolId` the account holds no pool for.
    const UNKNOWN_POOL_ID: &str = "ipam-pool-0000000000000000f";

    let rows: &[Refusal] = &[
        Refusal {
            case: "an id that is not an IPAM pool id at all",
            pool: &[],
            params: &[("IpamPoolId", "my-pool"), ("NetmaskLength", "24")],
            code: INVALID_IPAM_POOL_ID_MALFORMED,
            message: "Invalid id: \"my-pool\"",
        },
        Refusal {
            case: "a well-formed pool id the account holds no pool for",
            pool: &[],
            params: &[("IpamPoolId", UNKNOWN_POOL_ID), ("NetmaskLength", "24")],
            code: INVALID_IPAM_POOL_ID_NOT_FOUND,
            message: "The ipam-pool ID 'ipam-pool-0000000000000000f' does not exist",
        },
        Refusal {
            case: "a request naming both a range and a size",
            pool: &[],
            params: &[("Cidr", "10.0.1.0/24"), ("NetmaskLength", "24")],
            code: INVALID_PARAMETER_COMBINATION,
            message: "Only one of 'cidr' or 'netmaskLength' may be provided",
        },
        Refusal {
            case: "a request naming neither, on a pool with no default netmask length",
            pool: &[],
            params: &[],
            code: MISSING_PARAMETER,
            message: "Either 'cidr' or 'netmaskLength' must be provided",
        },
        Refusal {
            case: "an allowed range beside a request that names the range itself",
            pool: &[],
            params: &[("Cidr", "10.0.1.0/24"), ("AllowedCidr.1", "10.0.0.0/16")],
            code: INVALID_PARAMETER_COMBINATION,
            message: "allowedCidrs may be specified only when netmaskLength is provided",
        },
        Refusal {
            case: "a disallowed range beside a request that names the range itself",
            pool: &[],
            params: &[("Cidr", "10.0.1.0/24"), ("DisallowedCidr.1", "10.0.0.0/24")],
            code: INVALID_PARAMETER_COMBINATION,
            message: "disallowedCidrs may be specified only when netmaskLength is provided",
        },
        Refusal {
            case: "a range of the other address family",
            pool: &[],
            params: &[("Cidr", "2001:db8::/64")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '2001:db8::/64' for cidr. It must be an ipv4 CIDR block",
        },
        Refusal {
            case: "an allowed range that is not a CIDR block at all",
            pool: &[],
            params: &[("NetmaskLength", "24"), ("AllowedCidr.1", "ten-dot-zero")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value 'ten-dot-zero' for allowedCidrs. It must be an ipv4 CIDR \
                      block",
        },
        Refusal {
            case: "a range outside the space provisioned to the pool",
            pool: &[],
            params: &[("Cidr", "192.0.2.0/24")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '192.0.2.0/24' for cidr. It is not within a CIDR \
                      provisioned to the IPAM pool '{pool}'",
        },
        Refusal {
            case: "a size larger than the space the pool holds",
            pool: &[],
            params: &[("NetmaskLength", "8")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '8' for netmaskLength. The IPAM pool '{pool}' has no free \
                      CIDR of that size",
        },
        Refusal {
            case: "a size past what an IPv4 netmask can name",
            pool: &[],
            params: &[("NetmaskLength", "33")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '33' for netmaskLength. A netmask length for an ipv4 pool \
                      must be from 0 to 32",
        },
        Refusal {
            case: "a size below the pool's minimum netmask length",
            pool: &[
                ("AllocationMinNetmaskLength", "24"),
                ("AllocationMaxNetmaskLength", "28"),
            ],
            params: &[("NetmaskLength", "20")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '20' for netmaskLength. The IPAM pool '{pool}' allocates \
                      CIDRs with a netmask length of at least '24'",
        },
        Refusal {
            case: "a size above the pool's maximum netmask length",
            pool: &[
                ("AllocationMinNetmaskLength", "24"),
                ("AllocationMaxNetmaskLength", "28"),
            ],
            params: &[("NetmaskLength", "30")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '30' for netmaskLength. The IPAM pool '{pool}' allocates \
                      CIDRs with a netmask length of at most '28'",
        },
        Refusal {
            case: "a range whose own size is below the pool's minimum netmask length",
            pool: &[
                ("AllocationMinNetmaskLength", "24"),
                ("AllocationMaxNetmaskLength", "28"),
            ],
            params: &[("Cidr", "10.0.0.0/20")],
            code: INVALID_PARAMETER_VALUE,
            message: "Invalid value '20' for netmaskLength. The IPAM pool '{pool}' allocates \
                      CIDRs with a netmask length of at least '24'",
        },
        Refusal {
            case: "a tag specification naming another resource type",
            pool: &[],
            params: &[
                ("NetmaskLength", "24"),
                ("TagSpecification.1.ResourceType", "ipam-pool"),
                ("TagSpecification.1.Tag.1.Key", "Name"),
                ("TagSpecification.1.Tag.1.Value", "wrong"),
            ],
            code: INVALID_PARAMETER_VALUE,
            message: "'ipam-pool' is not a valid resource type for tagging on \
                      AllocateIpamPoolCidr",
        },
    ];

    for row in rows {
        let case = row.case;
        let (router, _engine) = emulator();
        let scope_id = create_scope(case, &router).await;
        let pool: Vec<Param> = row
            .pool
            .iter()
            .map(|(name, value)| param(name, value))
            .collect();
        let pool_id = create_pool(case, &router, &scope_id, ADDRESS_FAMILY_IPV4, &pool).await;
        provision(case, &router, &pool_id, "10.0.0.0/16").await;

        let mut params = vec![param("IpamPoolId", &pool_id)];
        for (name, value) in row.params {
            params.retain(|(held, _)| held != name);
            params.push(param(name, value));
        }
        refused(
            case,
            &router,
            ALLOCATE,
            &params,
            row.code,
            &row.message.replace("{pool}", &pool_id),
        )
        .await;

        // Nothing was stored for a refused request: the pool holds no allocation.
        let reported = held(case, &router, &pool_id).await;
        assert!(
            reported.is_empty(),
            "case {case:?}: a refused allocation must store nothing, got {reported:?}"
        );
    }
}

/// A dry run asks about PERMISSION and allocates nothing: every caller of this emulator is
/// permitted, so it reports that outcome and the pool is left as it was.
#[tokio::test]
async fn a_dry_run_allocates_nothing() {
    let case = "a dry run of an allocation";
    let (router, _scope_id, pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;

    let (status, body) = call(
        &router,
        ALLOCATE,
        &[
            param("IpamPoolId", &pool_id),
            param("NetmaskLength", "24"),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, DRY_RUN_OPERATION_STATUS,
        "case {case:?}: expected HTTP {DRY_RUN_OPERATION_STATUS}, got {status} with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, DRY_RUN_OPERATION,
        "case {case:?}: expected the code {DRY_RUN_OPERATION:?}, got {code:?} in {body}"
    );
    let message = error_message(&body);
    assert_eq!(
        message, DRY_RUN_OPERATION_MESSAGE,
        "case {case:?}: expected the message {DRY_RUN_OPERATION_MESSAGE:?}, got {message:?}"
    );

    let reported = held(case, &router, &pool_id).await;
    assert!(
        reported.is_empty(),
        "case {case:?}: a dry run must leave the pool with no allocation, got {reported:?}"
    );
}

// ---------------------------------------------------------------------------
// What the read of a pool's allocations selects
// ---------------------------------------------------------------------------

/// `GetIpamPoolAllocations` selects by the allocation id and by its filters, pages in id order,
/// and refuses an id nothing resolves.
#[tokio::test]
async fn a_read_of_the_pools_allocations_selects_and_pages() {
    let case = "three allocations of one pool";
    let (router, _scope_id, pool_id) =
        pool_with_space(case, ADDRESS_FAMILY_IPV4, "10.0.0.0/16").await;

    let mut allocations: Vec<ReportedAllocation> = Vec::new();
    for tier in ["web", "app", "data"] {
        let body = ok(
            case,
            &router,
            ALLOCATE,
            &[
                param("IpamPoolId", &pool_id),
                param("NetmaskLength", "24"),
                param("Description", tier),
            ],
        )
        .await;
        allocations.push(reported_one(case, &body));
    }

    // By id: the one allocation it names.
    let named = &allocations[1];
    let body = ok(
        case,
        &router,
        GET_ALLOCATIONS,
        &[
            param("IpamPoolId", &pool_id),
            param("IpamPoolAllocationId", &named.id),
        ],
    )
    .await;
    assert_eq!(
        reported_all(case, &body),
        vec![named.clone()],
        "case {case:?}: expected the allocation the id names, got {body}"
    );

    // By filter: each filter name the read resolves, over a member of the allocation.
    for (filter_case, name, value, expected) in [
        ("by range", "cidr", allocations[2].cidr.clone(), 1usize),
        ("by description", "description", "app".to_string(), 1),
        (
            "by resource type, which every allocation of this pool shares",
            "resource-type",
            IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM.to_string(),
            3,
        ),
        (
            "by a value nothing holds",
            "description",
            "nothing".to_string(),
            0,
        ),
    ] {
        let body = ok(
            filter_case,
            &router,
            GET_ALLOCATIONS,
            &[
                param("IpamPoolId", &pool_id),
                param("Filter.1.Name", name),
                param("Filter.1.Value.1", &value),
            ],
        )
        .await;
        let reported = reported_all(filter_case, &body);
        assert_eq!(
            reported.len(),
            expected,
            "case {filter_case:?}: expected {expected} allocation(s) for {name}={value:?}, got \
             {reported:?}"
        );
    }

    // An unknown filter is refused rather than ignored.
    refused(
        "a filter this read does not resolve",
        &router,
        GET_ALLOCATIONS,
        &[
            param("IpamPoolId", &pool_id),
            param("Filter.1.Name", "not-a-filter"),
            param("Filter.1.Value.1", "x"),
        ],
        INVALID_PARAMETER_VALUE,
        "The filter 'not-a-filter' is invalid",
    )
    .await;

    // An id of the right form that the pool holds no allocation for.
    refused(
        "an allocation id the pool holds nothing for",
        &router,
        GET_ALLOCATIONS,
        &[
            param("IpamPoolId", &pool_id),
            param("IpamPoolAllocationId", "ipam-pool-alloc-0000000000000000f"),
        ],
        "InvalidIpamPoolAllocationId.NotFound",
        "The ipam-pool-allocation ID 'ipam-pool-alloc-0000000000000000f' does not exist",
    )
    .await;

    // A pool the account does not hold is an error rather than an empty page.
    refused(
        "a read of a pool the account does not hold",
        &router,
        GET_ALLOCATIONS,
        &[param("IpamPoolId", "ipam-pool-0000000000000000f")],
        INVALID_IPAM_POOL_ID_NOT_FOUND,
        "The ipam-pool ID 'ipam-pool-0000000000000000f' does not exist",
    )
    .await;
}
