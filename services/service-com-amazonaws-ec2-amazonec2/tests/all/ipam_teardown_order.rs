// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the IPAM teardown path, driven over the wire the service actually
//! speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! Three rules are under test here, each asserted over a SET of inputs rather than one:
//!
//! 1. A refusal of a delete names the dependency that actually BLOCKS it, because a caller
//!    branches on that sentence to pick the call that clears the blocker: a pool other pools
//!    draw their space from is reported as that, a pool holding space of its own as the
//!    provisioned CIDR, and an IPAM as the SCOPE its pools are attached to — never as a
//!    dependency the caller cannot clear.
//! 2. A request's own parameter COMBINATION is answered before any id it names is read, so a
//!    `ProvisionIpamPoolCidr` carrying neither a `Cidr` nor a `NetmaskLength` is told what it
//!    has to carry whatever the pool id beside it looks like.
//! 3. A provisioned CIDR is reported by the opaque per-CIDR id AWS mints for it — the
//!    `ipam-pool-cidr` prefix and [`POOL_CIDR_ID_DIGITS`] hexadecimal digits, wider than an
//!    EC2 resource id — unique per record and stable across reads.
//!
//! Once every dependency has been taken away in order, each delete SUCCEEDS and the IPAM stops
//! resolving: the last test walks a whole teardown, which is the path a client's own teardown
//! takes.
//!
//! The rows of each table differ along what the rule generalises over: which dependency the
//! pool holds, which scope holds the blocking pool, the form of the id beside a request whose
//! combination of members is wrong, and the family, size and spelling of a provisioned range.
//!
//! Nothing here waits a fixed time for a state the clock owes: each look is taken again until
//! the read reports what the clock owed, or a deadline far past the service's own transition
//! span passes.

use std::time::{Duration, Instant};

use ec2::state::{
    ADDRESS_FAMILY_IPV4, ADDRESS_FAMILY_IPV6, DEPENDENCY_VIOLATION, INCORRECT_STATE,
    INVALID_IPAM_ID_NOT_FOUND, INVALID_IPAM_POOL_ID_NOT_FOUND, INVALID_PARAMETER_COMBINATION,
    IPAM_POOL_CIDR_ID_PREFIX, IPAM_STATE_DELETE_IN_PROGRESS, TRANSITION_DELAY,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message};
use super::wire::{Param, child_text, document, is_hex_id, is_long_id, param};

/// The Region every call here is signed for, which is therefore each IPAM's home Region.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The sentence EC2 answers a `ProvisionIpamPoolCidr` that carries neither of the two members
/// it had to carry one of — entirely fixed text, with nothing of the request echoed into it.
const MISSING_CIDR_AND_NETMASK: &str = "The request must contain either a CIDR or Netmask Length.";

/// How many hexadecimal digits an `IpamPoolCidr$IpamPoolCidrId` carries after its prefix:
/// written out here rather than read off the service's own constant, so a change to the width
/// the service mints fails this suite instead of being re-asserted by it.
const POOL_CIDR_ID_DIGITS: usize = 32;

/// The deadline the polls below give up at: a large multiple of the service's own transition
/// span, so an assertion is about the state the clock owes and never about how fast the host
/// runs.
fn settle_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long one look waits before looking again — a fraction of that span, so a poll inside the
/// deadline above costs many looks rather than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// The emulator the tests drive, as [`super::ec2_query::emulator`] builds it.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One call of `action` signed for the Region above.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One call of `action` requiring success, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
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
        "case {case:?}: expected the message {message:?} from {action}, got {actual_message:?}"
    );
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// One IPAM created over the wire, with the two default scopes it came up with.
#[derive(Debug, Clone)]
struct Ipam {
    id: String,
    private_scope: String,
    public_scope: String,
}

/// Create one IPAM and answer it with the scopes it came up with.
async fn create_ipam(case: &str, router: &MultiServiceRouter) -> Ipam {
    let body = ok(
        case,
        router,
        "CreateIpam",
        &[param("OperatingRegion.1.RegionName", REGION)],
    )
    .await;
    Ipam {
        id: member(case, &body, "ipamId"),
        private_scope: member(case, &body, "privateDefaultScopeId"),
        public_scope: member(case, &body, "publicDefaultScopeId"),
    }
}

/// Create one pool of `family` in `scope_id`, drawing on `source` when there is one and
/// carrying `extra` beside it, and answer the `IpamPoolId` the service minted.
async fn create_pool(
    case: &str,
    router: &MultiServiceRouter,
    scope_id: &str,
    family: &str,
    source: Option<&str>,
    extra: &[Param],
) -> String {
    let mut params = vec![
        param("IpamScopeId", scope_id),
        param("AddressFamily", family),
    ];
    if let Some(source_pool_id) = source {
        params.push(param("SourceIpamPoolId", source_pool_id));
    }
    params.extend(extra.iter().cloned());
    let body = ok(case, router, "CreateIpamPool", &params).await;
    member(case, &body, "ipamPoolId")
}

/// Every allocation `GetIpamPoolAllocations` reports for the pool, as `(cidr, resourceId)`
/// sorted by range — the space the pool reports as assigned.
async fn allocations(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
) -> Vec<(String, Option<String>)> {
    let body = ok(
        case,
        router,
        "GetIpamPoolAllocations",
        &[param("IpamPoolId", pool_id)],
    )
    .await;
    let parsed = document(case, &body);
    let mut reported: Vec<(String, Option<String>)> = parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolAllocationId").is_some())
        .map(|node| {
            (
                child_text(node, "cidr").unwrap_or_default(),
                child_text(node, "resourceId"),
            )
        })
        .collect();
    reported.sort();
    reported
}

/// Provision `cidr` to the pool and answer the `ipamPoolCidrId` the service minted for it.
async fn provision(case: &str, router: &MultiServiceRouter, pool_id: &str, cidr: &str) -> String {
    let body = ok(
        case,
        router,
        "ProvisionIpamPoolCidr",
        &[param("IpamPoolId", pool_id), param("Cidr", cidr)],
    )
    .await;
    member(case, &body, "ipamPoolCidrId")
}

/// Every `(ipamPoolCidrId, cidr, state)` the pool reports, sorted by range.
async fn held(
    case: &str,
    router: &MultiServiceRouter,
    pool_id: &str,
) -> Vec<(String, String, String)> {
    let body = ok(
        case,
        router,
        "GetIpamPoolCidrs",
        &[param("IpamPoolId", pool_id)],
    )
    .await;
    let parsed = document(case, &body);
    let mut reported: Vec<(String, String, String)> = parsed
        .descendants()
        .filter(|node| node.is_element() && child_text(*node, "ipamPoolCidrId").is_some())
        .map(|node| {
            (
                child_text(node, "ipamPoolCidrId").unwrap_or_default(),
                child_text(node, "cidr").unwrap_or_default(),
                child_text(node, "state").unwrap_or_default(),
            )
        })
        .collect();
    reported.sort_by(|left, right| left.1.cmp(&right.1));
    reported
}

/// Deprovision `cidr` from the pool, asserting the deprovision took.
async fn deprovision(case: &str, router: &MultiServiceRouter, pool_id: &str, cidr: &str) {
    ok(
        case,
        router,
        "DeprovisionIpamPoolCidr",
        &[param("IpamPoolId", pool_id), param("Cidr", cidr)],
    )
    .await;
}

/// Look at the pool's CIDRs AGAIN until it reports none at all — the removal the clock owes a
/// deprovision — or a deadline far past the service's own transition span passes.
///
/// The CONDITION is what is waited for, never a span of wall clock: the removal is one the clock
/// owes, so a loaded host costs more looks rather than a different answer.
async fn poll_until_empty(case: &str, router: &MultiServiceRouter, pool_id: &str) {
    let deadline = Instant::now() + settle_deadline();
    loop {
        let reported = held(case, router, pool_id).await;
        if reported.is_empty() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pool {pool_id} to report no CIDR once the \
             deprovisions had settled, got {reported:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// Look at the IPAM AGAIN until `DescribeIpams` for its id refuses `InvalidIpamId.NotFound` —
/// the removal the clock owed the delete — or that same deadline passes.
async fn poll_until_gone(case: &str, router: &MultiServiceRouter, ipam_id: &str) {
    let deadline = Instant::now() + settle_deadline();
    loop {
        let (status, body) = call(router, "DescribeIpams", &[param("IpamId.1", ipam_id)]).await;
        if status == REFUSAL_STATUS {
            assert_eq!(
                error_code(&body),
                INVALID_IPAM_ID_NOT_FOUND,
                "case {case:?}: expected the removed IPAM {ipam_id} to answer \
                 {INVALID_IPAM_ID_NOT_FOUND}, got {body}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {ipam_id} to stop resolving once its removal was due, got \
             HTTP {status} with {body}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

// ---------------------------------------------------------------------------
// Which dependency a refused delete names
// ---------------------------------------------------------------------------

/// What one row's pool still holds when the delete arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Holds {
    /// A pool created WITHIN it, which draws its space from it.
    ChildPool,
    /// A range provisioned to it, and nothing else.
    ProvisionedCidr,
    /// A range provisioned to it with an allocation carved out of that range.
    ProvisionedCidrAndAllocation,
    /// An allocation whose range has since been deprovisioned, so the pool holds the
    /// allocation alone.
    AllocationOnly,
}

/// The rule: a refused `DeleteIpamPool` names the dependency that actually blocks the delete,
/// so a caller reading the sentence calls the one operation that clears it.
///
/// The rows differ along WHICH dependency the pool holds — a pool that uses it as a source, a
/// range provisioned to it, that range with an allocation out of it, and an allocation whose
/// range has already gone — which is what "the blocking dependency" generalises over.
#[tokio::test]
async fn a_refused_pool_delete_names_the_dependency_that_blocks_it() {
    let rows: [(&str, Holds, &str); 4] = [
        (
            "a pool another pool draws on",
            Holds::ChildPool,
            "There are one or more pools that use the pool as a source.",
        ),
        (
            "a pool holding a provisioned range",
            Holds::ProvisionedCidr,
            "There are one or more pool cidrs provisioned in the pool.",
        ),
        (
            "a pool holding a provisioned range with an allocation out of it",
            Holds::ProvisionedCidrAndAllocation,
            "There are one or more pool cidrs provisioned in the pool.",
        ),
        (
            "a pool holding only an allocation",
            Holds::AllocationOnly,
            "There are one or more allocations in the pool.",
        ),
    ];

    for (case, holds, sentence) in rows {
        let router = emulator();
        let ipam = create_ipam(case, &router).await;
        let pool_id =
            create_pool(case, &router, &ipam.private_scope, ADDRESS_FAMILY_IPV4, None, &[]).await;
        let cidr = "10.0.0.0/16";
        match holds {
            Holds::ChildPool => {
                provision(case, &router, &pool_id, cidr).await;
                create_pool(
                    case,
                    &router,
                    &ipam.private_scope,
                    ADDRESS_FAMILY_IPV4,
                    Some(&pool_id),
                    &[],
                )
                .await;
            }
            Holds::ProvisionedCidr => {
                provision(case, &router, &pool_id, cidr).await;
            }
            Holds::ProvisionedCidrAndAllocation => {
                provision(case, &router, &pool_id, cidr).await;
                ok(
                    case,
                    &router,
                    "AllocateIpamPoolCidr",
                    &[param("IpamPoolId", &pool_id), param("NetmaskLength", "24")],
                )
                .await;
            }
            Holds::AllocationOnly => {
                provision(case, &router, &pool_id, cidr).await;
                ok(
                    case,
                    &router,
                    "AllocateIpamPoolCidr",
                    &[param("IpamPoolId", &pool_id), param("NetmaskLength", "24")],
                )
                .await;
                // The range stops being space the pool holds the instant its deprovision is
                // asked for, which leaves the allocation as the only dependency left.
                deprovision(case, &router, &pool_id, cidr).await;
            }
        }

        refused(
            case,
            &router,
            "DeleteIpamPool",
            &[param("IpamPoolId", &pool_id)],
            DEPENDENCY_VIOLATION,
            &format!("The specified action is not valid for {pool_id}. {sentence}"),
        )
        .await;
    }
}

/// The rule: a refused `DeleteIpam` names the SCOPE whose pools block the delete — the
/// container a caller has to empty — rather than the IPAM the request named.
///
/// The rows differ along which of the IPAM's two default scopes holds the pool.
#[tokio::test]
async fn a_refused_ipam_delete_names_the_scope_whose_pools_block_it() {
    for (case, public) in [
        ("a pool in the private default scope", false),
        ("a pool in the public default scope", true),
    ] {
        let router = emulator();
        let ipam = create_ipam(case, &router).await;
        let scope_id = if public {
            ipam.public_scope.clone()
        } else {
            ipam.private_scope.clone()
        };
        create_pool(case, &router, &scope_id, ADDRESS_FAMILY_IPV4, None, &[]).await;

        refused(
            case,
            &router,
            "DeleteIpam",
            &[param("IpamId", &ipam.id)],
            DEPENDENCY_VIOLATION,
            &format!(
                "The specified action is not valid for {scope_id}. The scope is either default \
                 scope or there are one or more pools attached to the scope."
            ),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// A request whose own members are wrong, whatever id sits beside them
// ---------------------------------------------------------------------------

/// Which pool id a row puts beside a provision that asks for no range at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolId {
    /// The id of a pool this account really holds.
    Existing,
    /// A well-formed id the account holds no pool for.
    Absent,
    /// A value that is no pool id at all.
    Malformed,
    /// A well-formed id of ANOTHER resource family.
    OtherFamily,
}

/// The rule: a request that carries neither of two members it had to carry one of is refused
/// `InvalidParameterCombination` with EC2's own sentence for that pair, and that is answered
/// before any id beside it is read — the request's own completeness is settled first.
///
/// The rows differ along the form of the pool id the request names, which is what "whatever id
/// sits beside the missing members" generalises over.
#[tokio::test]
async fn a_provision_asking_for_no_range_is_refused_before_its_pool_id_is_read() {
    for (case, which) in [
        ("a pool that exists", PoolId::Existing),
        ("a well-formed id naming no pool", PoolId::Absent),
        ("a value that is no pool id", PoolId::Malformed),
        ("an id of another resource family", PoolId::OtherFamily),
    ] {
        let router = emulator();
        let ipam = create_ipam(case, &router).await;
        let existing =
            create_pool(case, &router, &ipam.private_scope, ADDRESS_FAMILY_IPV4, None, &[]).await;
        let pool_id = match which {
            PoolId::Existing => existing,
            PoolId::Absent => "ipam-pool-0000000000000000f".to_string(),
            PoolId::Malformed => "not-a-pool".to_string(),
            PoolId::OtherFamily => "vpc-0a1b2c3d4e5f60718".to_string(),
        };

        refused(
            case,
            &router,
            "ProvisionIpamPoolCidr",
            &[param("IpamPoolId", &pool_id)],
            INVALID_PARAMETER_COMBINATION,
            MISSING_CIDR_AND_NETMASK,
        )
        .await;
    }
}

/// The rule: an IPAM pool id this account holds no pool for is reported as absent by the ONE
/// sentence EC2 uses for the kind, which names the resource by its WIRE TYPE token
/// (`ipam-pool`) rather than by prose — so every operation that resolves an `IpamPoolId`
/// words an unknown one identically.
///
/// The rows differ along the operation doing the resolving and the id the request names, which
/// is what "wherever an IpamPoolId is resolved" generalises over.
#[tokio::test]
async fn an_unknown_pool_is_named_by_its_wire_type_wherever_it_is_resolved() {
    /// One row: what the call is, the `Action` it drives, and the members beside the pool id.
    type Call = (&'static str, &'static str, &'static [(&'static str, &'static str)]);

    let absent = ["ipam-pool-0000000000000000f", "ipam-pool-0a1b2c3d4e5f60718"];
    let calls: [Call; 3] = [
        ("a read of the pool's CIDRs", "GetIpamPoolCidrs", &[]),
        (
            "a provision into the pool",
            "ProvisionIpamPoolCidr",
            &[("Cidr", "10.0.0.0/16")],
        ),
        (
            "a deprovision out of the pool",
            "DeprovisionIpamPoolCidr",
            &[("Cidr", "10.0.0.0/16")],
        ),
    ];

    for (what, action, extra) in calls {
        for pool_id in absent {
            let case = &format!("{what} for {pool_id}");
            let router = emulator();
            let mut params = vec![param("IpamPoolId", pool_id)];
            params.extend(extra.iter().map(|(name, value)| param(name, value)));

            refused(
                case,
                &router,
                action,
                &params,
                INVALID_IPAM_POOL_ID_NOT_FOUND,
                &format!("The ipam-pool ID '{pool_id}' does not exist"),
            )
            .await;
        }
    }
}

/// The rule: deprovisioning a range the pool holds no record for is refused for the STATE the
/// named resource is in, in EC2's own wording for that class, with the range copied from the
/// request.
///
/// The rows differ along the family and size of the range, and whether the pool holds another
/// range at all — what "a range this pool was never given" generalises over.
#[tokio::test]
async fn deprovisioning_a_range_the_pool_never_held_is_refused_for_its_state() {
    for (case, family, held_range, asked) in [
        (
            "a pool holding one other range",
            ADDRESS_FAMILY_IPV4,
            Some("10.0.0.0/16"),
            "10.99.0.0/24",
        ),
        (
            "a smaller range of another family",
            ADDRESS_FAMILY_IPV6,
            Some("2001:db8::/32"),
            "2001:db9::/48",
        ),
        (
            "a range of the pool's family in a pool holding one",
            ADDRESS_FAMILY_IPV4,
            Some("10.0.0.0/16"),
            "192.0.2.0/24",
        ),
    ] {
        let router = emulator();
        let ipam = create_ipam(case, &router).await;
        let pool_id =
            create_pool(case, &router, &ipam.private_scope, family, None, &[]).await;
        if let Some(range) = held_range {
            provision(case, &router, &pool_id, range).await;
        }

        refused(
            case,
            &router,
            "DeprovisionIpamPoolCidr",
            &[param("IpamPoolId", &pool_id), param("Cidr", asked)],
            INCORRECT_STATE,
            &format!("The specified action is not valid.The pool CIDR {asked} does not exist."),
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// The id a provisioned CIDR is reported by
// ---------------------------------------------------------------------------

/// The rule: every provisioned CIDR is reported by the opaque per-CIDR id AWS mints for it —
/// the `ipam-pool-cidr` prefix and [`POOL_CIDR_ID_DIGITS`] hexadecimal digits, which is WIDER
/// than the EC2 resource id form — unique across records and the same on a read back.
///
/// The rows differ along the family, the size and the spelling of the range provisioned, which
/// is what the minted id has to be independent of.
#[tokio::test]
async fn every_provisioned_cidr_carries_an_opaque_pool_cidr_id() {
    let rows: [(&str, &str, &[&str]); 3] = [
        (
            "two IPv4 ranges of different sizes",
            ADDRESS_FAMILY_IPV4,
            &["10.0.0.0/16", "192.0.2.0/24"],
        ),
        (
            "one IPv6 range",
            ADDRESS_FAMILY_IPV6,
            &["2001:db8::/32"],
        ),
        (
            "three IPv4 ranges, the last spelled with host bits set",
            ADDRESS_FAMILY_IPV4,
            &["10.1.0.0/16", "10.2.0.0/24", "10.3.0.7/24"],
        ),
    ];

    for (case, family, ranges) in rows {
        let router = emulator();
        let ipam = create_ipam(case, &router).await;
        let pool_id = create_pool(case, &router, &ipam.private_scope, family, None, &[]).await;
        let mut minted: Vec<String> = Vec::new();
        for range in ranges {
            let id = provision(case, &router, &pool_id, range).await;
            assert!(
                is_hex_id(IPAM_POOL_CIDR_ID_PREFIX, &id, POOL_CIDR_ID_DIGITS),
                "case {case:?}: expected {range} to be reported by an \
                 {IPAM_POOL_CIDR_ID_PREFIX} id of {POOL_CIDR_ID_DIGITS} hexadecimal \
                 digits, got {id:?}"
            );
            assert!(
                !is_long_id(IPAM_POOL_CIDR_ID_PREFIX, &id),
                "case {case:?}: expected an id wider than an EC2 resource id for {range}, got \
                 {id:?}"
            );
            assert!(
                !minted.contains(&id),
                "case {case:?}: expected a fresh id for {range}, got {id:?} which this store \
                 had already minted ({minted:?})"
            );
            minted.push(id);
        }

        // The ids a read reports are the ids the provisions answered, so a client that kept one
        // can still find its range — and they sort in the order the ranges were provisioned,
        // which is the order a page of them is reported in.
        let reported = held(case, &router, &pool_id).await;
        let mut read_ids: Vec<String> = reported.iter().map(|(id, _, _)| id.clone()).collect();
        read_ids.sort();
        assert_eq!(
            read_ids, minted,
            "case {case:?}: expected the read to report the ids the provisions answered, in \
             provisioning order, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The whole teardown
// ---------------------------------------------------------------------------

/// The rule, end to end: once each dependency has been taken away, every delete SUCCEEDS — the
/// child pool, then the pool it drew on, then the IPAM, which stops resolving once the removal
/// the clock owed it is due.
///
/// This is the path a client's own teardown takes, and the one each refusal above points at: the
/// sentence a refused delete carries names the call made next here.
#[tokio::test]
async fn a_teardown_in_dependency_order_deletes_every_ipam_resource() {
    let case = "an IPAM with a pool, a child pool, a range in each and a VPC out of the child";
    let router = emulator();
    let ipam = create_ipam(case, &router).await;
    let top_pool =
        create_pool(case, &router, &ipam.private_scope, ADDRESS_FAMILY_IPV4, None, &[]).await;
    provision(case, &router, &top_pool, "10.0.0.0/16").await;
    // The child pool allocates in the Region every call here is signed for, which is what lets a
    // VPC of that Region take its space out of it.
    let child_pool = create_pool(
        case,
        &router,
        &ipam.private_scope,
        ADDRESS_FAMILY_IPV4,
        Some(&top_pool),
        &[param("Locale", REGION)],
    )
    .await;
    provision(case, &router, &child_pool, "10.0.1.0/24").await;

    // A resource out of the child pool: the space is assigned to it for as long as it exists,
    // which is a dependency of the pool the teardown has to clear first.
    let vpc = ok(
        case,
        &router,
        "CreateVpc",
        &[
            param("Ipv4IpamPoolId", &child_pool),
            param("Ipv4NetmaskLength", "28"),
        ],
    )
    .await;
    let vpc_id = member(case, &vpc, "vpcId");
    let assigned = allocations(case, &router, &child_pool).await;
    assert!(
        assigned
            .iter()
            .any(|(_, resource)| resource.as_deref() == Some(vpc_id.as_str())),
        "case {case:?}: expected the child pool to report the VPC's allocation, got {assigned:?}"
    );

    // Out of order: the IPAM is refused for the scope its pools are attached to, and the pool
    // the child draws on for that child.
    refused(
        case,
        &router,
        "DeleteIpam",
        &[param("IpamId", &ipam.id)],
        DEPENDENCY_VIOLATION,
        &format!(
            "The specified action is not valid for {scope}. The scope is either default scope \
             or there are one or more pools attached to the scope.",
            scope = ipam.private_scope
        ),
    )
    .await;
    refused(
        case,
        &router,
        "DeleteIpamPool",
        &[param("IpamPoolId", &top_pool)],
        DEPENDENCY_VIOLATION,
        &format!(
            "The specified action is not valid for {top_pool}. There are one or more pools that \
             use the pool as a source."
        ),
    )
    .await;

    // In order: the VPC holding the child pool's space, the child's range, the child, the top
    // pool's range, the top pool, the IPAM.
    ok(case, &router, "DeleteVpc", &[param("VpcId", &vpc_id)]).await;
    let released = allocations(case, &router, &child_pool).await;
    assert!(
        released
            .iter()
            .all(|(_, resource)| resource.as_deref() != Some(vpc_id.as_str())),
        "case {case:?}: expected the deleted VPC's allocation to be released, got {released:?}"
    );
    deprovision(case, &router, &child_pool, "10.0.1.0/24").await;
    poll_until_empty(case, &router, &child_pool).await;
    ok(
        case,
        &router,
        "DeleteIpamPool",
        &[param("IpamPoolId", &child_pool)],
    )
    .await;
    deprovision(case, &router, &top_pool, "10.0.0.0/16").await;
    poll_until_empty(case, &router, &top_pool).await;
    ok(
        case,
        &router,
        "DeleteIpamPool",
        &[param("IpamPoolId", &top_pool)],
    )
    .await;

    let deleted = ok(case, &router, "DeleteIpam", &[param("IpamId", &ipam.id)]).await;
    assert_eq!(
        element(&deleted, "state").as_deref(),
        Some(IPAM_STATE_DELETE_IN_PROGRESS),
        "case {case:?}: expected the delete to answer the transient state, got {deleted}"
    );
    poll_until_gone(case, &router, &ipam.id).await;
}
