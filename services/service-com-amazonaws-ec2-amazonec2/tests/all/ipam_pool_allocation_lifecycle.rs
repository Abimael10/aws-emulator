// SPDX-License-Identifier: CC0-1.0

//! What a reported IPAM POOL ALLOCATION carries, and what RELEASING one does to the pool it
//! came out of — driven over `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! Two rules, each asserted over a set of inputs rather than over one recorded call:
//!
//! 1. EVERY allocation a response reports carries, beside the members the model declares for
//!    it, the elements the live EC2 wire renders on that shape: its id under `allocationId`,
//!    its range under `cidrBlock`, the provider of the space (`resourceProvider`) and its
//!    status. The rows differ along how the allocation came to be — a caller naming the range,
//!    a caller naming only a size, a VPC created out of the pool, a pool created within it —
//!    and along which operation reports it, because the rule is the SHAPE's and not one
//!    handler's.
//! 2. An allocation that is RELEASED stops holding its range at once: the pool reports no such
//!    allocation afterwards and hands the same space out again. The rows differ along what
//!    held the range (a caller, a VPC) and along how it was given back (`ReleaseIpamPoolAllocation`,
//!    deleting the resource that held it).
//!
//! A third table pins what a release that names nothing the pool holds is refused with, and
//! that every one of those refusals leaves the allocation exactly as it was.
//!
//! The suite keeps its own `call`/`ok`/`member` wrappers over the shared [`super::ec2_query`]
//! harness, which is how every other suite in this crate is arranged.

use ec2::state::{
    ADDRESS_FAMILY_IPV4, DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS,
    INVALID_IPAM_POOL_ALLOCATION_ID_MALFORMED, INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND,
    INVALID_IPAM_POOL_ID_MALFORMED, INVALID_IPAM_POOL_ID_NOT_FOUND, INVALID_PARAMETER_VALUE,
    IPAM_POOL_ALLOCATION_ID_DIGITS, IPAM_POOL_ALLOCATION_ID_PREFIX,
    IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM,
    IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL, IPAM_POOL_ALLOCATION_RESOURCE_TYPE_VPC,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, emulator, error_code, error_message};
use super::wire::{Param, child_text, document, is_hex_id, param};

/// The Region every call here is signed for, and the locale of every pool it creates: "the
/// locale is the Amazon Web Services Region where this IPAM pool is available for allocations"
/// (`AllocateIpamPoolCidr`), so a pool a VPC of this Region takes space out of has to name it.
const REGION: &str = "us-east-1";

/// The space provisioned to the pool every test here allocates out of.
const POOL_SPACE: &str = "10.123.0.0/16";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The release this suite drives.
const RELEASE: &str = "ReleaseIpamPoolAllocation";

/// The read every allocation of this suite is proved through.
const GET_ALLOCATIONS: &str = "GetIpamPoolAllocations";

/// The `resourceProvider` element the live wire renders on every allocation IPAM itself made.
const RESOURCE_PROVIDER_AWS: &str = "aws";

/// The `status` element a live allocation carries.
const STATUS_ACTIVE: &str = "active";

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

/// One IPAM pool of this Region with [`POOL_SPACE`] provisioned to it: the router, the scope
/// the pool lives in and the pool id.
async fn pool_with_space(case: &str) -> (MultiServiceRouter, String, String) {
    let (router, _engine) = emulator();
    let ipam = ok(case, &router, "CreateIpam", &[]).await;
    let scope_id = member(case, &ipam, "privateDefaultScopeId");
    let created = ok(
        case,
        &router,
        "CreateIpamPool",
        &[
            param("IpamScopeId", &scope_id),
            param("AddressFamily", ADDRESS_FAMILY_IPV4),
            param("Locale", REGION),
        ],
    )
    .await;
    let pool_id = member(case, &created, "ipamPoolId");
    ok(
        case,
        &router,
        "ProvisionIpamPoolCidr",
        &[param("IpamPoolId", &pool_id), param("Cidr", POOL_SPACE)],
    )
    .await;
    (router, scope_id, pool_id)
}

/// One reported allocation: the members the model declares for it, and the elements the live
/// wire renders beside them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAllocation {
    /// `ipamPoolAllocationId`, the modelled spelling of the allocation's id.
    id: String,
    /// `allocationId`, the second spelling of that same id.
    allocation_id: Option<String>,
    /// `cidr`, the modelled spelling of the range.
    cidr: String,
    /// `cidrBlock`, the second spelling of that same range.
    cidr_block: Option<String>,
    /// `resourceType`, what holds the range.
    resource_type: String,
    /// `resourceProvider`, who provides the space.
    resource_provider: Option<String>,
    /// `status`, whether the allocation is live.
    status: Option<String>,
}

/// Every allocation `body` carries, in the order it reported them.
fn reported_all(case: &str, body: &str) -> Vec<ReportedAllocation> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolAllocationId").is_some())
        .map(|node| ReportedAllocation {
            id: child_text(node, "ipamPoolAllocationId").unwrap_or_default(),
            allocation_id: child_text(node, "allocationId"),
            cidr: child_text(node, "cidr").unwrap_or_default(),
            cidr_block: child_text(node, "cidrBlock"),
            resource_type: child_text(node, "resourceType").unwrap_or_default(),
            resource_provider: child_text(node, "resourceProvider"),
            status: child_text(node, "status"),
        })
        .collect()
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

/// Assert that `allocation` carries every element the live wire renders on the shape, each
/// derived from the allocation itself.
fn assert_recorded_elements(case: &str, allocation: &ReportedAllocation) {
    assert_eq!(
        allocation.allocation_id.as_deref(),
        Some(allocation.id.as_str()),
        "case {case:?}: allocationId must repeat the allocation's own id, got {allocation:?}"
    );
    assert_eq!(
        allocation.cidr_block.as_deref(),
        Some(allocation.cidr.as_str()),
        "case {case:?}: cidrBlock must repeat the allocation's own range, got {allocation:?}"
    );
    assert_eq!(
        allocation.resource_provider.as_deref(),
        Some(RESOURCE_PROVIDER_AWS),
        "case {case:?}: expected the resourceProvider {RESOURCE_PROVIDER_AWS:?}, got \
         {allocation:?}"
    );
    assert_eq!(
        allocation.status.as_deref(),
        Some(STATUS_ACTIVE),
        "case {case:?}: expected the status {STATUS_ACTIVE:?}, got {allocation:?}"
    );
    assert!(
        is_hex_id(
            IPAM_POOL_ALLOCATION_ID_PREFIX,
            &allocation.id,
            IPAM_POOL_ALLOCATION_ID_DIGITS,
        ),
        "case {case:?}: expected an {IPAM_POOL_ALLOCATION_ID_PREFIX}- id of \
         {IPAM_POOL_ALLOCATION_ID_DIGITS} hexadecimal digits, got {allocation:?}"
    );
}

// ---------------------------------------------------------------------------
// What every reported allocation carries
// ---------------------------------------------------------------------------

/// How one allocation of the table below came to be.
#[derive(Debug, Clone, Copy)]
enum MadeBy {
    /// `AllocateIpamPoolCidr` naming the range itself.
    CallerNamingTheRange(&'static str),
    /// `AllocateIpamPoolCidr` naming only how large a range it wants.
    CallerNamingASize(&'static str),
    /// `CreateVpc` taking the VPC's CIDR block out of the pool.
    AVpc(&'static str),
    /// `CreateIpamPool` within the pool, with a range provisioned to it out of this pool's
    /// space — the other assignment `IpamPoolAllocation` names.
    APoolWithin(&'static str),
}

/// THE RULE: every allocation a response reports carries the elements the live wire renders on
/// that shape, each derived from the allocation itself.
///
/// The rows differ along how the allocation came to be, which is what decides the resource type
/// it carries and who asked for it; the elements are the same for all of them, because the rule
/// belongs to the shape.
#[tokio::test]
async fn every_reported_allocation_carries_the_elements_the_wire_renders() {
    // (case, how the allocation was made, the resource type it must then report)
    let rows: [(&str, MadeBy, &str); 4] = [
        (
            "a caller's allocation of a named range",
            MadeBy::CallerNamingTheRange("10.123.1.0/24"),
            IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM,
        ),
        (
            "a caller's allocation of a chosen range",
            MadeBy::CallerNamingASize("28"),
            IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM,
        ),
        (
            "the allocation a VPC holds",
            MadeBy::AVpc("20"),
            IPAM_POOL_ALLOCATION_RESOURCE_TYPE_VPC,
        ),
        (
            "the allocation a pool within this one holds",
            MadeBy::APoolWithin("18"),
            IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL,
        ),
    ];

    for (case, made_by, resource_type) in rows {
        let (router, scope_id, pool_id) = pool_with_space(case).await;
        match made_by {
            MadeBy::CallerNamingTheRange(cidr) => {
                let body = ok(
                    case,
                    &router,
                    "AllocateIpamPoolCidr",
                    &[param("IpamPoolId", &pool_id), param("Cidr", cidr)],
                )
                .await;
                // The rule reaches the operation that MAKES the allocation too, which reports
                // the same shape.
                let answered = reported_all(case, &body);
                assert_eq!(
                    answered.len(),
                    1,
                    "case {case:?}: expected one allocation in the answer, got {answered:?}"
                );
                assert_recorded_elements(case, &answered[0]);
            }
            MadeBy::CallerNamingASize(netmask_length) => {
                ok(
                    case,
                    &router,
                    "AllocateIpamPoolCidr",
                    &[
                        param("IpamPoolId", &pool_id),
                        param("NetmaskLength", netmask_length),
                    ],
                )
                .await;
            }
            MadeBy::AVpc(netmask_length) => {
                ok(
                    case,
                    &router,
                    "CreateVpc",
                    &[
                        param("Ipv4IpamPoolId", &pool_id),
                        param("Ipv4NetmaskLength", netmask_length),
                    ],
                )
                .await;
            }
            MadeBy::APoolWithin(netmask_length) => {
                let within = ok(
                    case,
                    &router,
                    "CreateIpamPool",
                    &[
                        param("IpamScopeId", &scope_id),
                        param("AddressFamily", ADDRESS_FAMILY_IPV4),
                        param("Locale", REGION),
                        param("SourceIpamPoolId", &pool_id),
                    ],
                )
                .await;
                let within_id = member(case, &within, "ipamPoolId");
                ok(
                    case,
                    &router,
                    "ProvisionIpamPoolCidr",
                    &[
                        param("IpamPoolId", &within_id),
                        param("NetmaskLength", netmask_length),
                    ],
                )
                .await;
            }
        }

        let reported = held(case, &router, &pool_id).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the pool to report one allocation, got {reported:?}"
        );
        let allocation = &reported[0];
        assert_eq!(
            allocation.resource_type, resource_type,
            "case {case:?}: expected the resource type {resource_type:?}, got {allocation:?}"
        );
        assert_recorded_elements(case, allocation);

        // An id this service minted is an id a request may NAME: the read that selects one
        // allocation resolves it rather than refusing it as malformed.
        let selected = ok(
            case,
            &router,
            GET_ALLOCATIONS,
            &[
                param("IpamPoolId", &pool_id),
                param("IpamPoolAllocationId", &allocation.id),
            ],
        )
        .await;
        assert_eq!(
            reported_all(case, &selected),
            vec![allocation.clone()],
            "case {case:?}: the minted id must select the allocation it names, got {selected}"
        );
    }
}

// ---------------------------------------------------------------------------
// What releasing an allocation does to the pool
// ---------------------------------------------------------------------------

/// How one allocation of the table below is given back.
#[derive(Debug, Clone, Copy)]
enum ReleasedBy {
    /// `ReleaseIpamPoolAllocation`, which names the pool, the allocation and its range.
    TheReleaseCall,
    /// Deleting the resource that held the range, which stops holding it with the resource.
    DeletingTheResource,
}

/// THE RULE: a released allocation stops holding its range at once — the pool reports no such
/// allocation afterwards, and the same space can be handed out again.
///
/// The rows differ along what held the range and how it was given back, because the rule is
/// about the ALLOCATION rather than about either caller.
#[tokio::test]
async fn a_released_allocation_leaves_the_pool_reporting_free_space() {
    // (case, whether a VPC holds the range, how it is released)
    let rows: [(&str, bool, ReleasedBy); 3] = [
        (
            "a caller's allocation, released",
            false,
            ReleasedBy::TheReleaseCall,
        ),
        (
            "the allocation a VPC holds, released",
            true,
            ReleasedBy::TheReleaseCall,
        ),
        (
            "the allocation a VPC holds, given back with the VPC",
            true,
            ReleasedBy::DeletingTheResource,
        ),
    ];

    for (case, held_by_a_vpc, released_by) in rows {
        let (router, _scope_id, pool_id) = pool_with_space(case).await;
        let vpc_id = if held_by_a_vpc {
            let body = ok(
                case,
                &router,
                "CreateVpc",
                &[
                    param("Ipv4IpamPoolId", &pool_id),
                    param("Ipv4NetmaskLength", "24"),
                ],
            )
            .await;
            Some(member(case, &body, "vpcId"))
        } else {
            ok(
                case,
                &router,
                "AllocateIpamPoolCidr",
                &[
                    param("IpamPoolId", &pool_id),
                    param("NetmaskLength", "24"),
                ],
            )
            .await;
            None
        };

        let before = held(case, &router, &pool_id).await;
        assert_eq!(
            before.len(),
            1,
            "case {case:?}: expected one allocation before the release, got {before:?}"
        );
        let taken = before[0].cidr.clone();

        match released_by {
            ReleasedBy::TheReleaseCall => {
                let body = ok(
                    case,
                    &router,
                    RELEASE,
                    &[
                        param("IpamPoolId", &pool_id),
                        param("IpamPoolAllocationId", &before[0].id),
                        param("Cidr", &taken),
                    ],
                )
                .await;
                assert_eq!(
                    element(&body, "success").as_deref(),
                    Some("true"),
                    "case {case:?}: a release that happened reports success, got {body}"
                );
            }
            ReleasedBy::DeletingTheResource => {
                let vpc_id = vpc_id
                    .clone()
                    .unwrap_or_else(|| panic!("case {case:?}: this row needs a VPC"));
                ok(
                    case,
                    &router,
                    "DeleteVpc",
                    &[param("VpcId", &vpc_id)],
                )
                .await;
            }
        }

        let after = held(case, &router, &pool_id).await;
        assert!(
            after.is_empty(),
            "case {case:?}: the pool must report no allocation after the release, got {after:?}"
        );

        // The space is the pool's to hand out again, which is what "released" means: the very
        // range that was taken is allocated a second time.
        let reallocated = ok(
            case,
            &router,
            "AllocateIpamPoolCidr",
            &[param("IpamPoolId", &pool_id), param("Cidr", &taken)],
        )
        .await;
        let reported = reported_all(case, &reallocated);
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected the released range to be allocatable again, got \
             {reallocated}"
        );
        assert_eq!(
            reported[0].cidr, taken,
            "case {case:?}: expected the released range {taken:?} back, got {reported:?}"
        );
        assert_ne!(
            reported[0].id, before[0].id,
            "case {case:?}: the second allocation of the range is a NEW allocation, got \
             {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// What a release that names nothing the pool holds answers
// ---------------------------------------------------------------------------

/// What one refusal row replaces in the request that would have succeeded.
#[derive(Debug, Clone, Copy)]
struct RefusalRow {
    /// The case this row is reported as.
    case: &'static str,
    /// The members to replace in the release request: an entry whose name the request already
    /// carries replaces it, and any other is added.
    replace: &'static [(&'static str, &'static str)],
    /// The error code the refusal must carry.
    code: &'static str,
    /// The HTTP status it must carry.
    status: u16,
}

/// THE RULE: a release has to name an allocation the pool actually holds, and the range that
/// allocation holds — anything else is refused, and the allocation is left exactly as it was.
///
/// The rows differ along WHICH part of the request does not resolve: the pool id's form, the
/// pool itself, the allocation id's form, the allocation itself, which pool holds it, and the
/// range it holds. The dry-run row is the request that WOULD have succeeded, which is what
/// makes its answer a report rather than a refusal of the request's contents.
#[tokio::test]
async fn a_release_naming_nothing_the_pool_holds_is_refused_and_changes_nothing() {
    let case = "a pool holding one allocation";
    let (router, scope_id, pool_id) = pool_with_space(case).await;
    ok(
        case,
        &router,
        "AllocateIpamPoolCidr",
        &[
            param("IpamPoolId", &pool_id),
            param("Cidr", "10.123.7.0/24"),
        ],
    )
    .await;
    let allocation = held(case, &router, &pool_id).await.remove(0);
    // A SECOND pool of the same IPAM, so one row can name an allocation another pool holds:
    // an allocation belongs to the pool it came out of, which is why a release names both.
    let other_pool_id = {
        let created = ok(
            case,
            &router,
            "CreateIpamPool",
            &[
                param("IpamScopeId", &scope_id),
                param("AddressFamily", ADDRESS_FAMILY_IPV4),
                param("Locale", REGION),
            ],
        )
        .await;
        let other_pool_id = member(case, &created, "ipamPoolId");
        ok(
            case,
            &router,
            "ProvisionIpamPoolCidr",
            &[
                param("IpamPoolId", &other_pool_id),
                param("Cidr", "10.124.0.0/16"),
            ],
        )
        .await;
        other_pool_id
    };

    let rows: [RefusalRow; 7] = [
        RefusalRow {
            case: "an id that is not an IPAM pool id at all",
            replace: &[("IpamPoolId", "my-pool")],
            code: INVALID_IPAM_POOL_ID_MALFORMED,
            status: REFUSAL_STATUS,
        },
        RefusalRow {
            case: "a pool the account does not hold",
            replace: &[("IpamPoolId", "ipam-pool-0123456789abcdef0")],
            code: INVALID_IPAM_POOL_ID_NOT_FOUND,
            status: REFUSAL_STATUS,
        },
        RefusalRow {
            case: "an id that is not an allocation id at all",
            replace: &[("IpamPoolAllocationId", "my-allocation")],
            code: INVALID_IPAM_POOL_ALLOCATION_ID_MALFORMED,
            status: REFUSAL_STATUS,
        },
        RefusalRow {
            case: "an allocation the pool does not hold",
            replace: &[("IpamPoolAllocationId", "ipam-pool-alloc-0123456789abcdef0")],
            code: INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND,
            status: REFUSAL_STATUS,
        },
        RefusalRow {
            case: "a range the allocation does not hold",
            replace: &[("Cidr", "10.123.8.0/24")],
            code: INVALID_PARAMETER_VALUE,
            status: REFUSAL_STATUS,
        },
        RefusalRow {
            case: "a value that is no CIDR block at all",
            replace: &[("Cidr", "ten dot nothing")],
            code: INVALID_PARAMETER_VALUE,
            status: REFUSAL_STATUS,
        },
        RefusalRow {
            case: "the request that would have succeeded, as a dry run",
            replace: &[("DryRun", "true")],
            code: DRY_RUN_OPERATION,
            status: DRY_RUN_OPERATION_STATUS,
        },
    ];

    for row in rows {
        let mut params = vec![
            param("IpamPoolId", &pool_id),
            param("IpamPoolAllocationId", &allocation.id),
            param("Cidr", &allocation.cidr),
        ];
        for (name, value) in row.replace {
            match params.iter_mut().find(|held| held.0 == *name) {
                Some(held) => held.1 = (*value).to_string(),
                None => params.push(param(name, value)),
            }
        }
        let (status, body) = call(&router, RELEASE, &params).await;
        assert_eq!(
            status, row.status,
            "case {case:?}/{row_case:?}: expected HTTP {expected}, got {status} with body \
             {body}",
            row_case = row.case,
            expected = row.status,
        );
        let answered = error_code(&body);
        assert_eq!(
            answered, row.code,
            "case {case:?}/{row_case:?}: expected the code {code:?}, got {answered:?} in \
             {body}",
            row_case = row.case,
            code = row.code,
        );
        let still = held(case, &router, &pool_id).await;
        assert_eq!(
            still,
            vec![allocation.clone()],
            "case {case:?}/{row_case:?}: a refused release must change nothing, got {still:?}",
            row_case = row.case,
        );
    }

    // The dry run above is the request that would have succeeded, and says so in AWS's own
    // sentence for that answer.
    let (_status, body) = call(
        &router,
        RELEASE,
        &[
            param("IpamPoolId", &pool_id),
            param("IpamPoolAllocationId", &allocation.id),
            param("Cidr", &allocation.cidr),
            param("DryRun", "true"),
        ],
    )
    .await;
    let message = error_message(&body);
    assert_eq!(
        message, DRY_RUN_OPERATION_MESSAGE,
        "case {case:?}: expected the dry-run sentence {DRY_RUN_OPERATION_MESSAGE:?}, got \
         {message:?}"
    );

    // The one sentence this operation words for itself is not AWS's recorded text (see the
    // handler's module docs), so what is pinned here is what the rule requires of it: it names
    // the value the REQUEST gave and the range the STORED allocation holds, so neither is
    // written into the message. The rows differ along which of the two the request got wrong.
    for (row_case, requested) in [
        ("a range the allocation does not hold", "10.123.8.0/24"),
        ("a range spelled with host bits set", "10.123.8.17/24"),
    ] {
        let (_status, body) = call(
            &router,
            RELEASE,
            &[
                param("IpamPoolId", &pool_id),
                param("IpamPoolAllocationId", &allocation.id),
                param("Cidr", requested),
            ],
        )
        .await;
        let message = error_message(&body);
        for named in [requested, allocation.id.as_str(), allocation.cidr.as_str()] {
            assert!(
                message.contains(named),
                "case {case:?}/{row_case:?}: the sentence must name {named:?} — the request's \
                 own value, the allocation and the range it holds — got {message:?}"
            );
        }
    }
    let (_status, body) = call(
        &router,
        RELEASE,
        &[
            param("IpamPoolId", &pool_id),
            param("IpamPoolAllocationId", &allocation.id),
            param("Cidr", "ten dot nothing"),
        ],
    )
    .await;
    let message = error_message(&body);
    assert!(
        message.contains("ten dot nothing"),
        "case {case:?}: a value that is no CIDR block at all is named back to the caller, got \
         {message:?}"
    );
    assert!(
        !message.contains(&allocation.cidr),
        "case {case:?}: a value that is no CIDR block names no range of the allocation, so the \
         sentence does not report one, got {message:?}"
    );

    // And an allocation THIS pool holds is none of the other pool's: a release that names it
    // through the other pool is refused the way an unknown id is, so one pool can never give
    // back another's space.
    let (status, body) = call(
        &router,
        RELEASE,
        &[
            param("IpamPoolId", &other_pool_id),
            param("IpamPoolAllocationId", &allocation.id),
            param("Cidr", &allocation.cidr),
        ],
    )
    .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} for another pool's allocation, got \
         {status} with body {body}"
    );
    let answered = error_code(&body);
    assert_eq!(
        answered, INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND,
        "case {case:?}: an allocation of another pool is not this pool's to release: expected \
         {INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND:?}, got {answered:?} in {body}"
    );
    let still = held(case, &router, &pool_id).await;
    assert_eq!(
        still,
        vec![allocation.clone()],
        "case {case:?}: that refusal must leave the allocation where it is, got {still:?}"
    );
}
